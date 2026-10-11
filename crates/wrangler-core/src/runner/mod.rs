//! The runner interface and the shared process plumbing behind it.

pub mod claude;
pub mod codex;
pub mod mock;

use crate::model::{Experiment, FactorKind, Isolation, NetworkPolicy, RunnerCfg, Task, Variant};
use crate::transcript::Transcript;
use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How well a runner honors one factor kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Support {
    Full,
    /// Honored through a different mechanism; the note says how.
    Approximate(&'static str),
    Unsupported(&'static str),
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub overlay: Support,
    pub model: Support,
    pub effort: Support,
    pub system_prompt_append: Support,
    pub knob: Support,
    pub seatbelt: Support,
    /// Whether the runner reports dollar cost.
    pub reports_cost: bool,
}

impl Capabilities {
    pub fn for_kind(&self, kind: FactorKind) -> &Support {
        match kind {
            FactorKind::Overlay => &self.overlay,
            FactorKind::Model => &self.model,
            FactorKind::Effort => &self.effort,
            FactorKind::SystemPromptAppend => &self.system_prompt_append,
            FactorKind::Knob => &self.knob,
            FactorKind::Runner => &Support::Full,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
    pub isolation: Isolation,
    pub network: NetworkPolicy,
    pub cfg: RunnerCfg,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunEvent {
    /// rate_limit | retry | timeout | error | sandbox | note
    pub kind: String,
    /// Milliseconds since the run started.
    pub at_ms: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct RunRecord {
    pub transcript: Transcript,
    /// Raw runner stdout (stream-json or JSONL), kept verbatim.
    pub raw_output: String,
    pub stderr: String,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub wall_ms: u64,
    pub metrics: BTreeMap<String, f64>,
    pub final_text: String,
    pub events: Vec<RunEvent>,
    /// Set when the run was rejected by a rate or usage limit and is worth retrying.
    pub rate_limited: Option<String>,
    /// Set when the runner reported a failure.
    pub error: Option<String>,
    /// Set when the agent was stopped by a configured limit (max turns). The
    /// run is graded as it stands, like a timeout.
    pub limit_hit: Option<String>,
    /// Model the runner reported using.
    pub model: String,
    pub command: Vec<String>,
}

pub trait AgentRunner: Send + Sync {
    /// "claude-code", "codex", ...
    fn id(&self) -> &str;
    /// Which factor kinds it can honor.
    fn capabilities(&self) -> Capabilities;
    /// Write the variant's overlay into the workspace the way this runner discovers it.
    fn materialize(&self, ws: &Workspace, variant: &Variant) -> Result<()>;
    fn run(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Result<RunRecord>;
}

pub fn get(id: &str) -> Result<Arc<dyn AgentRunner>> {
    match id {
        "claude-code" | "claude" => Ok(Arc::new(claude::ClaudeCodeRunner::default())),
        "codex" => Ok(Arc::new(codex::CodexRunner::default())),
        "mock" => Ok(Arc::new(mock::MockRunner)),
        other => bail!("unknown runner `{other}` (available: claude-code, codex, mock)"),
    }
}

pub fn known() -> Vec<&'static str> {
    vec!["claude-code", "codex", "mock"]
}

/// Check every variant's runner against the factor kinds in use. Unsupported
/// combinations are errors; approximations come back as warnings.
pub fn capability_warnings(exp: &Experiment, variants: &[Variant]) -> Result<Vec<String>> {
    let mut warnings = BTreeSet::new();
    let runners: BTreeSet<&str> = variants.iter().map(|v| v.runner.as_str()).collect();
    for rid in runners {
        let runner = get(rid)?;
        let caps = runner.capabilities();
        for f in &exp.factors {
            match caps.for_kind(f.kind) {
                Support::Full => {}
                Support::Approximate(note) => {
                    warnings.insert(format!("runner `{rid}` approximates factor `{}` ({}): {note}", f.name, f.kind.as_str()));
                }
                Support::Unsupported(why) => {
                    bail!("runner `{rid}` cannot honor factor `{}` ({}): {why}", f.name, f.kind.as_str())
                }
            }
        }
        if exp.isolation == Isolation::Seatbelt {
            match &caps.seatbelt {
                Support::Full => {}
                Support::Approximate(note) => {
                    warnings.insert(format!("runner `{rid}` under seatbelt isolation: {note}"));
                }
                Support::Unsupported(why) => bail!("runner `{rid}` cannot run under seatbelt isolation: {why}"),
            }
            if !cfg!(target_os = "macos") {
                bail!("seatbelt isolation is only available on macOS");
            }
        }
        if !caps.reports_cost {
            warnings.insert(format!("runner `{rid}` does not report dollar cost; cost comparisons exclude its runs"));
        }
    }
    Ok(warnings.into_iter().collect())
}

// ------------------------------------------------------------ process exec

pub struct ExecSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Default)]
pub struct ExecOut {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub wall_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

/// Run a process in its own process group with stdin closed, stdout and
/// stderr captured to files, a hard timeout, and cooperative cancellation.
pub fn exec(spec: &ExecSpec) -> Result<ExecOut> {
    use std::os::unix::process::CommandExt;
    let stdout = std::fs::File::create(&spec.stdout_path)?;
    let stderr = std::fs::File::create(&spec.stderr_path)?;
    let mut cmd = std::process::Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0);
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().with_context(|| format!("starting `{}` (is it installed and on PATH?)", spec.program))?;
    let pid = child.id() as i32;
    let mut out = ExecOut::default();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() >= spec.timeout {
            out.timed_out = true;
        } else if spec.cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
        }
        if out.timed_out || out.cancelled {
            // SAFETY: plain signal delivery to the process group we created.
            unsafe { libc::killpg(pid, libc::SIGTERM) };
            let grace = Instant::now();
            let st = loop {
                if let Some(st) = child.try_wait()? {
                    break st;
                }
                if grace.elapsed() > Duration::from_secs(5) {
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                    break child.wait()?;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            break Some(st);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Reap anything the agent left running in the group.
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    out.exit = status.and_then(|s| s.code());
    out.wall_ms = start.elapsed().as_millis() as u64;
    out.stdout = String::from_utf8_lossy(&std::fs::read(&spec.stdout_path).unwrap_or_default()).to_string();
    out.stderr = String::from_utf8_lossy(&std::fs::read(&spec.stderr_path).unwrap_or_default()).to_string();
    Ok(out)
}

/// Does this text look like a rate or usage limit rejection?
pub fn looks_rate_limited(text: &str) -> bool {
    let t = text.to_lowercase();
    ["rate limit", "rate_limit", "usage limit", "limit reached", "too many requests", "overloaded", "429", "529"]
        .iter()
        .any(|needle| t.contains(needle))
}

pub fn tally_tool_calls(transcript: &Transcript, metrics: &mut BTreeMap<String, f64>) {
    let mut total = 0.0;
    for c in transcript.tool_calls() {
        total += 1.0;
        *metrics.entry(format!("tool.{}", c.name)).or_insert(0.0) += 1.0;
    }
    metrics.insert("tool_calls".into(), total);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_times_out_and_kills_group() {
        let tmp = tempfile::tempdir().unwrap();
        let out = exec(&ExecSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "echo started; sleep 30".into()],
            cwd: tmp.path().to_path_buf(),
            env: vec![],
            env_remove: vec![],
            stdout_path: tmp.path().join("o"),
            stderr_path: tmp.path().join("e"),
            timeout: Duration::from_millis(400),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
        assert!(out.timed_out);
        assert!(out.wall_ms < 6000);
        assert_eq!(out.stdout.trim(), "started");
    }

    #[test]
    fn rate_limit_phrases() {
        assert!(looks_rate_limited("Claude usage limit reached. Resets at 5pm"));
        assert!(looks_rate_limited("API Error: 429 {\"type\":\"rate_limit_error\"}"));
        assert!(!looks_rate_limited("wrote the summary"));
    }
}

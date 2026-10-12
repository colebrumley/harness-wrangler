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
    /// MCP servers given to the agent for a variant.
    pub mcp: Support,
    pub seatbelt: Support,
    /// Whether hook executions are visible in the runner's output.
    pub hooks: Support,
    /// Whether the runner can hold a multi-turn dialog with a simulated user.
    pub conversation: Support,
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
            FactorKind::Mcp => &self.mcp,
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
    /// Conversation tasks: the model (and effort) that plays the user.
    pub simulator: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunEvent {
    /// rate_limit | retry | timeout | error | sandbox | hook | note
    pub kind: String,
    /// Milliseconds since the run started.
    pub at_ms: u64,
    pub detail: String,
    /// Structured payload. Hook events carry `event`, `matcher`,
    /// `exit_code`, `outcome` and `output`.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
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
    /// Conversation tasks: the simulator's configuration and how the dialog
    /// ended, stored with the run. Null otherwise.
    pub conversation: serde_json::Value,
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

/// Run a process like `exec`, but with stdin held open for a dialog over
/// newline-delimited JSON. `first` is written at once. Each time a line of
/// the given `turn_end` type appears on stdout, `on_turn` is called with
/// everything printed so far and returns the next line to write, or `None`
/// to close stdin and let the process finish.
pub fn exec_dialog(spec: &ExecSpec, first: &str, turn_end: &str, on_turn: &mut dyn FnMut(&str) -> Result<Option<String>>) -> Result<ExecOut> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    let stdout = std::fs::File::create(&spec.stdout_path)?;
    let stderr = std::fs::File::create(&spec.stderr_path)?;
    let mut cmd = std::process::Command::new(&spec.program);
    cmd.args(&spec.args).current_dir(&spec.cwd).stdin(std::process::Stdio::piped()).stdout(stdout).stderr(stderr).process_group(0);
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().with_context(|| format!("starting `{}` (is it installed and on PATH?)", spec.program))?;
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take();
    let send = |stdin: &mut Option<std::process::ChildStdin>, line: &str| -> bool {
        match stdin.as_mut() {
            Some(pipe) => writeln!(pipe, "{}", line.trim_end()).and_then(|_| pipe.flush()).is_ok(),
            None => false,
        }
    };
    send(&mut stdin, first);
    let mut out = ExecOut::default();
    let mut seen_turns = 0usize;
    let mut callback_err: Option<anyhow::Error> = None;
    let read = |p: &PathBuf| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).to_string();
    // After stdin closes the process should exit on its own; give it a moment before insisting.
    let mut closed_at: Option<Instant> = None;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() >= spec.timeout {
            out.timed_out = true;
        } else if spec.cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
        }
        let lingering = closed_at.map(|t| t.elapsed() > Duration::from_secs(20)).unwrap_or(false);
        if out.timed_out || out.cancelled || lingering || callback_err.is_some() {
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
        if stdin.is_some() {
            let so_far = read(&spec.stdout_path);
            // Only complete lines count: the last one may still be being written.
            let complete = &so_far[..so_far.rfind('\n').map(|i| i + 1).unwrap_or(0)];
            let turns = complete.lines().filter(|l| serde_json::from_str::<serde_json::Value>(l).map(|v| v["type"] == turn_end).unwrap_or(false)).count();
            if turns > seen_turns {
                seen_turns = turns;
                match on_turn(complete) {
                    Ok(Some(next)) => {
                        if !send(&mut stdin, &next) {
                            stdin = None;
                            closed_at = Some(Instant::now());
                        }
                    }
                    Ok(None) => {
                        stdin = None;
                        closed_at = Some(Instant::now());
                    }
                    Err(e) => callback_err = Some(e),
                }
                continue;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    drop(stdin);
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    if let Some(e) = callback_err {
        return Err(e);
    }
    out.exit = status.and_then(|s| s.code());
    out.wall_ms = start.elapsed().as_millis() as u64;
    out.stdout = read(&spec.stdout_path);
    out.stderr = read(&spec.stderr_path);
    Ok(out)
}

/// Does this text look like a rate or usage limit rejection?
pub fn looks_rate_limited(text: &str) -> bool {
    let t = text.to_lowercase();
    ["rate limit", "rate_limit", "usage limit", "limit reached", "too many requests", "overloaded", "429", "529"]
        .iter()
        .any(|needle| t.contains(needle))
}

/// The servers of an MCP config file: name → definition.
pub fn mcp_servers(path: &std::path::Path) -> Result<Vec<(String, serde_json::Value)>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading MCP config {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("MCP config {} is not JSON", path.display()))?;
    let Some(servers) = v["mcpServers"].as_object() else { bail!("MCP config {} has no `mcpServers` object", path.display()) };
    Ok(servers.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Every MCP config a variant brings into a run: its `mcp` factor levels,
/// then a `.mcp.json` an overlay left at the workspace root.
pub fn mcp_config_paths(ws: &Workspace, variant: &Variant) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = variant.mcp_configs().iter().map(|c| ws.project.abs(c)).collect();
    let from_overlay = ws.dir.join(".mcp.json");
    if from_overlay.is_file() {
        out.push(from_overlay);
    }
    out
}

pub fn tally_tool_calls(transcript: &Transcript, metrics: &mut BTreeMap<String, f64>) {
    let mut total = 0.0;
    let mut distinct = BTreeSet::new();
    for c in transcript.tool_calls() {
        total += 1.0;
        distinct.insert(c.name.as_str());
        // MCP tools are already `mcp.<server>.<tool>`; everything else is `tool.<Name>`.
        let key = if c.name.starts_with("mcp.") { c.name.clone() } else { format!("tool.{}", c.name) };
        *metrics.entry(key).or_insert(0.0) += 1.0;
    }
    metrics.insert("tool_calls".into(), total);
    metrics.insert("tool_calls_before_final".into(), transcript.calls_before_final() as f64);
    metrics.insert("distinct_tools".into(), distinct.len() as f64);
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
    fn exec_dialog_feeds_turns_until_told_to_stop() {
        let tmp = tempfile::tempdir().unwrap();
        // A stand-in agent: answers each stdin line with an echo and a turn-end marker.
        let script = r#"while IFS= read -r line; do echo "{\"type\":\"echo\",\"got\":$line}"; echo '{"type":"result"}'; done; echo '{"type":"bye"}'"#;
        let spec = ExecSpec { program: "sh".into(), args: vec!["-c".into(), script.into()], cwd: tmp.path().to_path_buf(), env: vec![], env_remove: vec![], stdout_path: tmp.path().join("o"), stderr_path: tmp.path().join("e"), timeout: Duration::from_secs(20), cancel: Arc::new(AtomicBool::new(false)) };
        let mut sent = vec![];
        let out = exec_dialog(&spec, "1", "result", &mut |so_far| {
            let turns = so_far.lines().filter(|l| l.contains("\"result\"")).count();
            sent.push(turns);
            Ok((turns < 3).then(|| (turns + 1).to_string()))
        })
        .unwrap();
        assert_eq!(sent, vec![1, 2, 3]);
        let got: Vec<&str> = out.stdout.lines().filter(|l| l.contains("echo")).collect();
        assert_eq!(got, vec![r#"{"type":"echo","got":1}"#, r#"{"type":"echo","got":2}"#, r#"{"type":"echo","got":3}"#]);
        // Closing stdin let the process finish on its own.
        assert!(out.stdout.trim_end().ends_with(r#"{"type":"bye"}"#) && !out.timed_out && out.exit == Some(0));
        // A callback error stops the process and surfaces.
        let err = exec_dialog(&spec, "1", "result", &mut |_| anyhow::bail!("simulator failed")).unwrap_err();
        assert!(err.to_string().contains("simulator failed"));
    }

    #[test]
    fn rate_limit_phrases() {
        assert!(looks_rate_limited("Claude usage limit reached. Resets at 5pm"));
        assert!(looks_rate_limited("API Error: 429 {\"type\":\"rate_limit_error\"}"));
        assert!(!looks_rate_limited("wrote the summary"));
    }
}

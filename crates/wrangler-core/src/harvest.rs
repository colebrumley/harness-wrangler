//! Task harvesting: turn a Claude Code session, or a wrangler run, into a
//! draft task. The tool does the deterministic part (the prompt, a fixture
//! snapshot, the test commands the session ran, a diff bound) and leaves
//! the judgment (expectations, whether the task is any good) to the user or
//! an agent. `docs/harvesting.md` lists the session fields relied on.

use crate::graders::judge::{backend_for, JudgeBackend};
use crate::model::{DetSpec, Fixture, JudgeCfg, Task};
use crate::project::Project;
use crate::store::Store;
use crate::util::{slug, truncate_middle};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Files that are never copied into a fixture unless named with `--include`.
pub const SECRET_PATTERNS: &[&str] = &[".env*", "*.pem", "*_key*", "id_rsa*", "credentials*"];
pub const DEFAULT_MAX_MB: u64 = 50;
/// Shell commands recognized as running a project's tests.
pub const TEST_PATTERNS: &[&str] = &["pytest", "cargo test", "npm test", "go test", "unittest"];

// ----------------------------------------------------------------- session

#[derive(Debug, Clone, Default)]
pub struct ToolUse {
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub enum Entry {
    /// Something the person typed.
    Prompt { text: String, at: String },
    Assistant { text: String, calls: Vec<ToolUse> },
    ToolResult { content: String },
}

/// A Claude Code session file, reduced to what harvesting needs.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub version: String,
    pub git_branch: String,
    /// Timestamp of the first entry that carries one.
    pub started_at: String,
    pub entries: Vec<Entry>,
}

fn strip_reminders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(a) = rest.find("<system-reminder>") {
        out.push_str(&rest[..a]);
        match rest[a..].find("</system-reminder>") {
            Some(b) => rest = &rest[a + b + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Text the harness wrote into a user turn, not the person.
fn is_harness_text(text: &str) -> bool {
    let t = text.trim_start();
    ["<command-name>", "<command-message>", "<local-command-", "<task-notification>", "[Request interrupted", "Caveat: The messages below"].iter().any(|p| t.starts_with(p))
}

impl Session {
    /// Parse a session JSONL. The format is undocumented and changes between
    /// versions, so anything unexpected is an error that names the line.
    pub fn parse(text: &str) -> Result<Session> {
        let mut s = Session::default();
        let mut lines = 0;
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            lines += 1;
            let ev: Value = serde_json::from_str(line).with_context(|| format!("line {}: not JSON; this does not look like a Claude Code session file", n + 1))?;
            let Some(ty) = ev["type"].as_str() else { bail!("line {}: entry has no `type`; unsupported session format", n + 1) };
            let field = |k: &str| ev[k].as_str().unwrap_or("").to_string();
            if s.id.is_empty() {
                s.id = field("sessionId");
            }
            if s.started_at.is_empty() {
                s.started_at = field("timestamp");
            }
            if ty != "user" && ty != "assistant" {
                continue;
            }
            if s.cwd.is_empty() {
                s.cwd = field("cwd");
                s.version = field("version");
                s.git_branch = field("gitBranch");
            }
            // Subagent transcripts are not part of the conversation with the person.
            if ev["isSidechain"].as_bool().unwrap_or(false) {
                continue;
            }
            let content = &ev["message"]["content"];
            if content.is_null() {
                bail!("line {}: `{ty}` entry has no message.content; unsupported session format (Claude Code {})", n + 1, if s.version.is_empty() { "version unknown" } else { &s.version });
            }
            let blocks: Vec<Value> = match content {
                Value::String(t) => vec![json!({"type": "text", "text": t})],
                Value::Array(a) => a.clone(),
                other => bail!("line {}: message.content is {}; expected a string or a list of blocks", n + 1, if other.is_object() { "an object" } else { "a scalar" }),
            };
            if ty == "assistant" {
                let text = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n");
                let calls = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| ToolUse { name: b["name"].as_str().unwrap_or("").to_string(), input: b["input"].clone() }).collect();
                s.entries.push(Entry::Assistant { text, calls });
                continue;
            }
            for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                let content = match &b["content"] {
                    Value::String(t) => t.clone(),
                    Value::Array(items) => items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                };
                s.entries.push(Entry::ToolResult { content });
            }
            let text = strip_reminders(&blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"));
            if !text.is_empty() && !ev["isMeta"].as_bool().unwrap_or(false) && !is_harness_text(&text) {
                s.entries.push(Entry::Prompt { text, at: field("timestamp") });
            }
        }
        if lines == 0 {
            bail!("the session file is empty");
        }
        if s.cwd.is_empty() {
            bail!("no entry records a `cwd`; unsupported session format");
        }
        if s.prompts().is_empty() {
            bail!("the session has no user message to use as a prompt");
        }
        Ok(s)
    }

    /// Find a session file by path, or by id under `~/.claude/projects/`.
    pub fn locate(reference: &str) -> Result<PathBuf> {
        let p = Path::new(reference);
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        let root = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/projects");
        let name = format!("{}.jsonl", reference.trim_end_matches(".jsonl"));
        let found: Vec<PathBuf> = std::fs::read_dir(&root).map(|rd| rd.flatten().map(|e| e.path().join(&name)).filter(|f| f.is_file()).collect()).unwrap_or_default();
        match found.len() {
            1 => Ok(found[0].clone()),
            0 => bail!("`{reference}` is not a file and no session with that id is under {}", root.display()),
            _ => bail!("session id `{reference}` matches {} files; pass the path", found.len()),
        }
    }

    /// The person's messages, in order, with each one's index in `entries`.
    pub fn prompts(&self) -> Vec<(usize, &str, &str)> {
        self.entries.iter().enumerate().filter_map(|(i, e)| if let Entry::Prompt { text, at } = e { Some((i, text.as_str(), at.as_str())) } else { None }).collect()
    }

    fn calls_after(&self, entry: usize) -> impl Iterator<Item = &ToolUse> {
        self.entries.iter().skip(entry).flat_map(|e| match e {
            Entry::Assistant { calls, .. } => calls.iter().collect::<Vec<_>>(),
            _ => vec![],
        })
    }

    /// Test commands the session ran after the given entry, cleaned up and deduplicated.
    pub fn test_commands(&self, entry: usize) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for c in self.calls_after(entry).filter(|c| c.name == "Bash") {
            let Some(cmd) = c.input["command"].as_str() else { continue };
            if let Some(t) = test_command(cmd, &self.cwd) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// Lines the session wrote or replaced after the given entry: a rough
    /// measure of how big the session's own change was.
    pub fn diff_lines(&self, entry: usize) -> u64 {
        let lines = |v: &Value| v.as_str().map(|s| s.lines().count() as u64).unwrap_or(0);
        self.calls_after(entry)
            .map(|c| match c.name.as_str() {
                "Edit" => lines(&c.input["old_string"]) + lines(&c.input["new_string"]),
                "MultiEdit" => c.input["edits"].as_array().into_iter().flatten().map(|e| lines(&e["old_string"]) + lines(&e["new_string"])).sum(),
                "Write" => lines(&c.input["content"]),
                _ => 0,
            })
            .sum()
    }

    /// Files the session edited (`Edit`) or created (`Write`), relative to its cwd.
    pub fn touched_files(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = vec![];
        for c in self.calls_after(0).filter(|c| matches!(c.name.as_str(), "Edit" | "MultiEdit" | "Write" | "NotebookEdit")) {
            let Some(p) = c.input["file_path"].as_str().or(c.input["notebook_path"].as_str()) else { continue };
            let rel = Path::new(p).strip_prefix(&self.cwd).map(|r| r.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string());
            let edited = c.name != "Write";
            match out.iter_mut().find(|(f, _)| *f == rel) {
                // An Edit before any Write means the file was there when the session touched it.
                Some(_) => {}
                None => out.push((rel, edited)),
            }
        }
        out
    }

    /// A commit hash the session's own git commands printed, if any: the
    /// first `git rev-parse HEAD` or `git log` output.
    pub fn revealed_head(&self) -> Option<String> {
        let sha = regex::Regex::new(r"(?m)^(?:commit )?([0-9a-f]{7,40})\b").unwrap();
        let mut awaiting = false;
        for e in &self.entries {
            match e {
                Entry::Assistant { calls, .. } => {
                    awaiting = calls.iter().any(|c| c.name == "Bash" && c.input["command"].as_str().map(|s| s.contains("git rev-parse HEAD") || s.contains("git log")).unwrap_or(false));
                }
                Entry::ToolResult { content } if awaiting => {
                    if let Some(m) = sha.captures(content) {
                        return Some(m[1].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The conversation after the prompt, for drafting expectations.
    pub fn digest(&self, entry: usize, max_chars: usize) -> String {
        let mut s = String::new();
        for e in self.entries.iter().skip(entry + 1) {
            match e {
                Entry::Prompt { text, .. } => s.push_str(&format!("[user] {}\n", text.trim())),
                Entry::Assistant { text, calls } => {
                    if !text.trim().is_empty() {
                        s.push_str(&format!("[agent] {}\n", text.trim()));
                    }
                    for c in calls {
                        s.push_str(&format!("[agent ran {}] {}\n", c.name, crate::transcript::input_summary(&c.input, 160)));
                    }
                }
                Entry::ToolResult { .. } => {}
            }
        }
        truncate_middle(&s, max_chars)
    }
}

/// The test invocation inside a shell command, if it runs one: the segment
/// between `&&`, `;` and `|` that matches a known pattern, with the session's
/// own directory stripped so it runs from the fixture root.
pub fn test_command(command: &str, cwd: &str) -> Option<String> {
    let cleaned = command.replace(&format!("{}/", cwd.trim_end_matches('/')), "").replace(cwd, ".");
    cleaned
        .split("&&")
        .flat_map(|s| s.split(';'))
        .map(|s| s.split('|').next().unwrap_or("").trim())
        .find(|seg| TEST_PATTERNS.iter().any(|p| seg.contains(p)) && !seg.starts_with("cd ") && !seg.starts_with("echo "))
        .map(|seg| seg.trim_end_matches("2>&1").trim().to_string())
        .filter(|s| !s.is_empty())
}

// ----------------------------------------------------------------- fixture

#[derive(Debug, Clone, Default, Serialize)]
pub struct FixtureReport {
    /// Project-relative directory the fixture was written to.
    pub dir: String,
    /// `commit`: exported from a commit. `current`: a copy of the directory as it is now.
    pub source: String,
    pub commit: Option<String>,
    /// How the commit was found: `session` (printed by a git command in the
    /// session) or `date` (the last commit before the session started).
    pub commit_from: Option<String>,
    pub files: usize,
    pub bytes: u64,
    /// Secret-looking files that were left out.
    pub secrets_skipped: Vec<String>,
    /// Files the session edited that are not in the commit: they were
    /// uncommitted when the session started, and the fixture holds their
    /// content as it is now.
    pub working_tree_files: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FixtureOptions {
    pub max_bytes: u64,
    /// Globs that override the secret list, matched against the file name and the relative path.
    pub include: Vec<String>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        FixtureOptions { max_bytes: DEFAULT_MAX_MB * 1024 * 1024, include: vec![] }
    }
}

fn matches_any(patterns: &[impl AsRef<str>], rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    patterns.iter().any(|p| glob::Pattern::new(p.as_ref()).map(|g| g.matches(name) || g.matches(rel)).unwrap_or(false))
}

pub fn is_secret(rel: &str, include: &[String]) -> bool {
    matches_any(SECRET_PATTERNS, rel) && !matches_any(include, rel)
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").current_dir(dir).args(args).stdin(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn over_cap(bytes: u64, opts: &FixtureOptions) -> Result<()> {
    if bytes > opts.max_bytes {
        bail!("the fixture would be {:.1} MB, above the {} MB cap; nothing was copied. Raise it with --max-mb, or harvest from a smaller directory.", bytes as f64 / 1_048_576.0, opts.max_bytes / 1_048_576);
    }
    Ok(())
}

/// Snapshot a session's repository into `dest`. In order of preference: the
/// commit the session started from, exported with `git archive`; else a copy
/// of the directory as it is now, without gitignored files.
pub fn capture_fixture(cwd: &Path, commit: Option<(&str, &str)>, edited: &[(String, bool)], dest: &Path, opts: &FixtureOptions) -> Result<FixtureReport> {
    if !cwd.is_dir() {
        bail!("the session's directory {} no longer exists, so there is nothing to snapshot", cwd.display());
    }
    if dest.exists() {
        bail!("{} already exists; remove it or harvest under another id", dest.display());
    }
    let mut rep = FixtureReport::default();
    let top = git_out(cwd, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()));
    // A commit is only usable when it exists in this repository.
    let commit = commit.filter(|(sha, _)| top.is_some() && git_out(cwd, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some());
    match (top, commit) {
        (Some(top), Some((sha, from))) => {
            if top.canonicalize().ok() != cwd.canonicalize().ok() {
                rep.warnings.push(format!("the session ran in a subdirectory ({}); the fixture is the whole repository, so the agent starts at its root", cwd.strip_prefix(&top).unwrap_or(cwd).display()));
            }
            let listing = git_out(&top, &["ls-tree", "-r", "-l", "-z", sha]).ok_or_else(|| anyhow!("git ls-tree {sha} failed"))?;
            // "<mode> <type> <hash> <size>\t<path>" per entry.
            let entries: Vec<(u64, String)> = listing.split('\0').filter_map(|e| e.split_once('\t')).map(|(meta, path)| (meta.split_whitespace().nth(3).and_then(|s| s.parse().ok()).unwrap_or(0), path.to_string())).collect();
            let keep: Vec<&(u64, String)> = entries.iter().filter(|(_, p)| !is_secret(p, &opts.include)).collect();
            rep.secrets_skipped = entries.iter().filter(|(_, p)| is_secret(p, &opts.include)).map(|(_, p)| p.clone()).collect();
            over_cap(keep.iter().map(|(n, _)| *n).sum(), opts)?;
            std::fs::create_dir_all(dest)?;
            let archive = Command::new("git").current_dir(&top).args(["archive", "--format=tar", sha]).stdout(std::process::Stdio::piped()).spawn().context("running git archive")?;
            let untar = Command::new("tar").arg("-x").arg("-C").arg(dest).stdin(archive.stdout.context("git archive has no output")?).status().context("running tar")?;
            if !untar.success() {
                bail!("extracting commit {sha} failed");
            }
            for p in &rep.secrets_skipped {
                let _ = std::fs::remove_file(dest.join(p));
            }
            // Files the session edited that the commit does not have were
            // uncommitted at the start. Their content then is gone; copy what is there now.
            let in_commit: std::collections::HashSet<&str> = entries.iter().map(|(_, p)| p.as_str()).collect();
            let prefix = cwd.strip_prefix(&top).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            for (rel, was_edit) in edited.iter().filter(|(_, e)| *e) {
                let _ = was_edit;
                let in_repo = if prefix.is_empty() { rel.clone() } else { format!("{prefix}/{rel}") };
                let src = top.join(&in_repo);
                if in_commit.contains(in_repo.as_str()) || !src.is_file() || Path::new(rel).is_absolute() {
                    continue;
                }
                if is_secret(&in_repo, &opts.include) {
                    rep.secrets_skipped.push(in_repo);
                    continue;
                }
                let dst = dest.join(&in_repo);
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::copy(&src, &dst)?;
                rep.working_tree_files.push(in_repo);
            }
            if !rep.working_tree_files.is_empty() {
                rep.warnings.push(format!("{} file(s) the session edited are not in commit {}: they were uncommitted when it started. The fixture holds them as they are NOW, after the session; check them: {}", rep.working_tree_files.len(), &sha[..sha.len().min(10)], rep.working_tree_files.join(", ")));
            }
            rep.source = "commit".into();
            rep.commit = Some(sha.to_string());
            rep.commit_from = Some(from.to_string());
            if from == "date" {
                rep.warnings.push(format!("the session file does not record its starting commit; {} is the last commit before the session began. If the session started from uncommitted work, the fixture is missing it.", &sha[..sha.len().min(10)]));
            }
        }
        (top, _) => {
            // Current state: tracked and untracked files, gitignored ones left out.
            let files: Vec<String> = match &top {
                Some(_) => git_out(cwd, &["ls-files", "-co", "--exclude-standard", "-z"]).unwrap_or_default().split('\0').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect(),
                None => walkdir::WalkDir::new(cwd).into_iter().filter_entry(|e| e.file_name() != ".git").flatten().filter(|e| e.file_type().is_file()).filter_map(|e| e.path().strip_prefix(cwd).ok().map(|p| p.to_string_lossy().to_string())).collect(),
            };
            let (secret, keep): (Vec<String>, Vec<String>) = files.into_iter().filter(|f| cwd.join(f).is_file()).partition(|f| is_secret(f, &opts.include));
            rep.secrets_skipped = secret;
            over_cap(keep.iter().filter_map(|f| cwd.join(f).metadata().ok()).map(|m| m.len()).sum(), opts)?;
            for f in &keep {
                let dst = dest.join(f);
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::copy(cwd.join(f), &dst).with_context(|| format!("copying {f}"))?;
            }
            std::fs::create_dir_all(dest)?;
            rep.source = "current".into();
            rep.warnings.push("this fixture is the directory's CURRENT state, not its state when the session started: the starting commit could not be determined. If the session's work is still in the tree, the task is already done in the fixture.".into());
        }
    }
    for e in walkdir::WalkDir::new(dest).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        rep.files += 1;
        rep.bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
    }
    if !rep.secrets_skipped.is_empty() {
        rep.secrets_skipped.sort();
        rep.secrets_skipped.dedup();
    }
    Ok(rep)
}

// ------------------------------------------------------------------ import

#[derive(Debug, Clone, Default)]
pub struct SessionImport {
    /// 1-based index among the person's messages. Default: the first.
    pub turn: Option<usize>,
    /// Draft `expected_output` and `expectations` with this model.
    pub draft_expectations: Option<String>,
    pub include: Vec<String>,
    pub max_mb: Option<u64>,
    /// Task and fixture id. Default: `<project>-<session id prefix>`.
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Harvested {
    pub task_id: String,
    /// Task file, relative to the project root.
    pub task_file: String,
    pub prompt: String,
    pub fixture: Option<FixtureReport>,
    pub test_commands: Vec<String>,
    pub diff_max: Option<u64>,
    pub drafted_expectations: bool,
    pub cost_usd: f64,
    pub warnings: Vec<String>,
}

fn draft_schema() -> Value {
    json!({"type": "object", "properties": {"expected_output": {"type": "string"}, "expectations": {"type": "array", "items": {"type": "string"}}}, "required": ["expected_output", "expectations"]})
}

fn draft_prompt(prompt: &str, digest: &str) -> String {
    format!(
        "A person gave a coding agent the task below, and the session that followed is summarised after it. Draft what a GOOD result of this task looks like, so the task can be given to other agents and graded.\n\n<task_prompt>\n{}\n</task_prompt>\n\n<session>\n{}\n</session>\n\nReturn `expected_output`: one or two sentences describing a good result, in terms of the outcome and not of the steps this agent took. Return `expectations`: three to six specific, checkable statements about a good result. Draw them from what the agent ended up doing and what the person accepted or corrected. Do not require things only this session happened to do.",
        prompt.trim(),
        digest
    )
}

fn write_task(project: &Project, rel: &str, header: &str, task: &Task) -> Result<()> {
    let path = project.abs(rel);
    if path.exists() {
        bail!("{rel} already exists; remove it or harvest under another id");
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, format!("{header}{}", toml::to_string_pretty(task)?))?;
    Ok(())
}

/// Write a draft task from a Claude Code session file.
pub fn import_session(project: &Project, session_file: &Path, opts: &SessionImport, cancel: &Arc<AtomicBool>) -> Result<Harvested> {
    let text = std::fs::read_to_string(session_file).with_context(|| format!("reading {}", session_file.display()))?;
    let session = Session::parse(&text).with_context(|| format!("in {}", session_file.display()))?;
    let prompts = session.prompts();
    let turn = opts.turn.unwrap_or(1);
    let Some(&(entry, prompt, at)) = turn.checked_sub(1).and_then(|i| prompts.get(i)) else { bail!("the session has {} user message(s); --turn {turn} is out of range", prompts.len()) };
    let project_name = Path::new(&session.cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "session".into());
    let short_id: String = session.id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    let id = opts.id.clone().map(|s| slug(&s)).unwrap_or_else(|| slug(&format!("{project_name}-{}{}", if short_id.is_empty() { "session" } else { &short_id }, if turn > 1 { format!("-t{turn}") } else { String::new() })));
    let mut warnings = vec![];

    // The repository as it was: a commit the session revealed, else the last one before it began.
    let cwd = Path::new(&session.cwd);
    let commit: Option<(String, &str)> = session.revealed_head().filter(|_| turn == 1).map(|sha| (sha, "session")).or_else(|| {
        let before = if at.is_empty() { session.started_at.as_str() } else { at };
        (!before.is_empty()).then(|| git_out(cwd, &["rev-list", "-1", &format!("--before={before}"), "HEAD"])).flatten().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).map(|sha| (sha, "date"))
    });
    if turn > 1 {
        warnings.push(format!("--turn {turn}: the fixture is the repository before the session, not as it was at turn {turn}. Work the agent did in earlier turns is not in it."));
    }
    let fixture_rel = format!("fixtures/{id}");
    let fx_opts = FixtureOptions { max_bytes: opts.max_mb.unwrap_or(DEFAULT_MAX_MB) * 1024 * 1024, include: opts.include.clone() };
    let fixture = capture_fixture(cwd, commit.as_ref().map(|(s, f)| (s.as_str(), *f)), &session.touched_files(), &project.abs(&fixture_rel), &fx_opts)?;
    let fixture = FixtureReport { dir: fixture_rel.clone(), ..fixture };

    let test_commands = session.test_commands(entry);
    let session_diff = session.diff_lines(entry);
    let diff_max = (session_diff > 0).then(|| (session_diff * 2).max(10));
    let mut graders: Vec<DetSpec> = test_commands.iter().take(3).enumerate().map(|(i, cmd)| DetSpec::Command { cmd: cmd.clone(), expect_exit: 0, timeout_secs: None, name: Some(if i == 0 { "tests-pass".to_string() } else { format!("tests-pass-{}", i + 1) }) }).collect();
    if let Some(max) = diff_max {
        graders.push(DetSpec::DiffSize { min: None, max: Some(max), name: None });
    }
    let mut task = Task { id: id.clone(), prompt: prompt.to_string(), fixture: Some(Fixture { dir: Some(fixture_rel), repo: None, git_ref: None }), tags: vec!["harvested".into(), "draft".into(), slug(&project_name)], ..Default::default() };
    task.graders.deterministic = graders;
    let mut cost = 0.0;
    if let Some(model) = &opts.draft_expectations {
        let cfg = JudgeCfg { model: model.clone(), effort: None, rubric: None, rubric_scoring: false, pairwise: false, pairwise_mode: None, pairwise_reps: None, transcript: None };
        let backend: Box<dyn JudgeBackend> = backend_for(&cfg);
        let reply = backend.complete(&draft_prompt(prompt, &session.digest(entry, 40_000)), &draft_schema(), &cfg, cancel)?;
        task.expected_output = reply.value["expected_output"].as_str().unwrap_or("").trim().to_string();
        task.expectations = reply.value["expectations"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        cost = reply.cost_usd;
    }
    let date = at.split('T').next().unwrap_or("");
    let header = format!(
        "# source: Claude Code session {}, turn {turn}{}{}\n# Harvested by `wrangler import session`. A draft: read the prompt, the fixture and the graders, then remove the `draft` tag.\n",
        if session.id.is_empty() { "(no id)" } else { &session.id },
        if date.is_empty() { String::new() } else { format!(", {date}") },
        if session.version.is_empty() { String::new() } else { format!(" (Claude Code {})", session.version) }
    );
    let task_file = format!("tasks/harvested/{id}.toml");
    if let Err(e) = write_task(project, &task_file, &header, &task) {
        let _ = std::fs::remove_dir_all(project.abs(&fixture.dir));
        return Err(e);
    }
    warnings.extend(fixture.warnings.iter().cloned());
    if test_commands.is_empty() {
        warnings.push("the session ran no recognized test command, so no `command` grader was proposed; add one, or expectations for the judge".into());
    }
    Ok(Harvested { task_id: id, task_file, prompt: prompt.to_string(), fixture: Some(fixture), test_commands, diff_max, drafted_expectations: opts.draft_expectations.is_some(), cost_usd: cost, warnings })
}

/// Write a draft task from a wrangler run: the prompt, fixture and graders
/// are the original task's, and the run's output becomes a reference the
/// rubric judge is shown.
pub fn import_run(project: &Project, store: &Store, run_id: i64, id: Option<&str>) -> Result<Harvested> {
    let run = store.run(run_id)?;
    let launch = store.launch(run.launch_id)?;
    let original = launch.tasks.iter().find(|t| t.id == run.task_id).with_context(|| format!("run {run_id} references a task the launch no longer has"))?;
    if run.output_text.trim().is_empty() {
        bail!("run {run_id} has no output to use as a reference (status: {})", run.status);
    }
    let id = id.map(slug).unwrap_or_else(|| slug(&format!("{}-from-run-{run_id}", original.id)));
    let mut task = original.clone();
    task.id = id.clone();
    task.source = String::new();
    task.prompt_file = None;
    task.reference_output = Some(truncate_middle(run.output_text.trim(), 20_000));
    // Graders the experiment added at load (trigger checks) are not the task's own.
    task.graders.deterministic.retain(|g| !g.name().map(|n| n.starts_with("trigger:")).unwrap_or(false));
    for tag in ["harvested", "draft"] {
        if !task.tags.iter().any(|t| t == tag) {
            task.tags.push(tag.to_string());
        }
    }
    let variant = launch.variants.iter().find(|v| v.key == run.variant_key).map(|v| v.label.clone()).unwrap_or_default();
    let header = format!(
        "# source: wrangler run {run_id} (launch #{} of {}, task {}, variant {variant}){}\n# Harvested by `wrangler import run`. The run's output is the reference the judge sees. A draft: review it, then remove the `draft` tag.\n",
        launch.id,
        launch.experiment,
        original.id,
        run.ended_at.as_deref().and_then(|d| d.split('T').next()).map(|d| format!(", {d}")).unwrap_or_default()
    );
    let task_file = format!("tasks/harvested/{id}.toml");
    write_task(project, &task_file, &header, &task)?;
    let test_commands = task.graders.deterministic.iter().filter_map(|g| if let DetSpec::Command { cmd, .. } = g { Some(cmd.clone()) } else { None }).collect();
    Ok(Harvested { task_id: id, task_file, prompt: task.prompt.clone(), fixture: None, test_commands, diff_max: None, drafted_expectations: false, cost_usd: 0.0, warnings: vec![] })
}

// ------------------------------------------------------------- task check

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub reps: Option<u32>,
    /// Borrow the runner, model, isolation and graders of this experiment.
    pub experiment: Option<String>,
    pub runner: Option<String>,
    pub model: Option<String>,
    pub judge_model: Option<String>,
    pub rubric: Option<String>,
}

/// Find a task by id or by file path.
pub fn find_task(project: &Project, reference: &str) -> Result<Task> {
    let path = project.abs(reference);
    if reference.ends_with(".toml") && path.is_file() {
        return project.load_task_file(&path);
    }
    let mut broken = vec![];
    for t in project.list_all_tasks() {
        match t {
            Ok(t) if t.id == reference => return Ok(t),
            Ok(_) => {}
            Err(e) => broken.push(format!("{e:#}")),
        }
    }
    bail!("no task with id `{reference}` under tasks/{}", if broken.is_empty() { String::new() } else { format!(" ({} task file(s) do not parse: {})", broken.len(), broken[0]) })
}

/// The experiment a task check runs: this one task on the baseline alone.
/// No factors, so the only variant is the runner's defaults with no overlay.
pub fn check_experiment(project: &Project, task: &Task, opts: &CheckOptions) -> Result<crate::model::Experiment> {
    use crate::model::*;
    let mut exp = match &opts.experiment {
        Some(name) => project.load_experiment(name)?,
        None => Experiment::parse("name = \"task-check\"\n")?,
    };
    exp.name = format!("task-check-{}", task.id);
    exp.description = format!("Baseline check of task {}: does it leave headroom, and how much does it vary?", task.id);
    exp.runs_per_cell = opts.reps.unwrap_or(3).max(1);
    // The baseline is "no pattern": every pattern-like factor is dropped, and
    // the others collapse to their first level.
    let first = |kind: FactorKind| exp.factors.iter().find(|f| f.kind == kind).map(|f| f.levels[0].clone());
    if let Some(l) = first(FactorKind::Runner) {
        exp.runner.kind = l.value().to_string();
        exp.runner.model = l.extra("model").map(|s| s.to_string()).or(exp.runner.model.take());
    }
    if let Some(l) = first(FactorKind::Model) {
        exp.runner.model = Some(l.value().to_string());
    }
    if let Some(l) = first(FactorKind::Effort) {
        exp.runner.effort = Some(l.value().to_string());
    }
    exp.factors.clear();
    exp.ablation = None;
    exp.trigger = None;
    exp.primary_metric = None;
    exp.tasks = TaskSel { include: vec![task.source.clone()], exclude: vec![] };
    if let Some(r) = &opts.runner {
        exp.runner.kind = r.clone();
    }
    if let Some(m) = &opts.model {
        exp.runner.model = Some(m.clone());
    }
    if opts.judge_model.is_some() || opts.rubric.is_some() {
        let mut defaults = exp.default_graders();
        let mut judge = defaults.judge.take().unwrap_or(JudgeCfg { model: String::new(), effort: None, rubric: None, rubric_scoring: true, pairwise: false, pairwise_mode: None, pairwise_reps: None, transcript: None });
        if let Some(m) = &opts.judge_model {
            judge.model = m.clone();
        }
        if opts.rubric.is_some() {
            judge.rubric = opts.rubric.clone();
        }
        if judge.model.is_empty() {
            bail!("--rubric needs a judge: pass --judge-model, or --experiment with a [graders.default] judge");
        }
        judge.pairwise = false;
        defaults.judge = Some(judge);
        exp.graders.insert("default".into(), defaults);
    } else if let Some(d) = exp.graders.get_mut("default") {
        // One variant has nothing to compare pairwise.
        if let Some(j) = d.judge.as_mut() {
            j.pairwise = false;
        }
    }
    if task.source.is_empty() {
        bail!("task `{}` has no file under the project, so it cannot be checked", task.id);
    }
    Ok(exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_test_commands() {
        let cwd = "/Users/dev/orders";
        let t = |c: &str| test_command(c, cwd);
        assert_eq!(t("python3 -m unittest discover -s tests -q").as_deref(), Some("python3 -m unittest discover -s tests -q"));
        assert_eq!(t("cd /Users/dev/orders && python3 -m pytest -q 2>&1 | tail -20").as_deref(), Some("python3 -m pytest -q"));
        assert_eq!(t("cargo test -p core --lib; echo done").as_deref(), Some("cargo test -p core --lib"));
        assert_eq!(t("npm test -- --watch=false").as_deref(), Some("npm test -- --watch=false"));
        assert_eq!(t("go test /Users/dev/orders/pkg/...").as_deref(), Some("go test pkg/..."));
        assert_eq!(t("ls tests && cat tests/test_x.py"), None);
        assert_eq!(t("echo 'run pytest later'"), None);
    }

    #[test]
    fn secret_patterns_and_overrides() {
        for f in [".env", ".env.local", "config/.env.production", "server.pem", "deploy_key", "api_key.txt", "id_rsa", "id_rsa.pub", "credentials.json", "aws/credentials"] {
            assert!(is_secret(f, &[]), "{f} should be treated as a secret");
        }
        for f in ["environment.md", "src/main.rs", "keys.md", "README.md"] {
            assert!(!is_secret(f, &[]), "{f} is not a secret");
        }
        assert!(!is_secret(".env.example", &[".env.example".into()]));
        assert!(!is_secret("config/.env.example", &["*.example".into()]));
        assert!(is_secret(".env", &[".env.example".into()]));
    }

    #[test]
    fn parser_fails_loudly_on_unknown_shapes() {
        let err = |text: &str| format!("{:#}", Session::parse(text).unwrap_err());
        assert!(err("not json at all").contains("line 1: not JSON"));
        assert!(err("{\"no_type\": 1}").contains("no `type`"));
        assert!(err("{\"type\":\"user\",\"cwd\":\"/x\",\"version\":\"9.9.9\"}").contains("no message.content; unsupported session format (Claude Code 9.9.9)"));
        assert!(err("{\"type\":\"user\",\"cwd\":\"/x\",\"message\":{\"content\":{\"odd\":true}}}").contains("expected a string or a list of blocks"));
        assert!(err("{\"type\":\"summary\"}").contains("no entry records a `cwd`"));
        assert!(err("").contains("empty"));
        assert!(err("{\"type\":\"assistant\",\"cwd\":\"/x\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}").contains("no user message"));
    }
}

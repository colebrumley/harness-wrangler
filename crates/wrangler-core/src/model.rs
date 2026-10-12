//! Core definitions: experiments, factors, variants, tasks, grader specs.
//!
//! Everything here is plain data parsed from the TOML files in the user's
//! project repo. Paths inside definitions are relative to the project root.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_RUNS_PER_CELL: u32 = 5;
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    #[default]
    Workspace,
    Seatbelt,
    Docker,
}

/// What a sandboxed run may reach. Written `"full"`, `"anthropic"`, or
/// `{ allow = ["anthropic", "localhost:8080", "registry.npmjs.org"] }`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// No network restriction.
    #[default]
    Full,
    /// Outbound traffic only through an allowlisting proxy (Anthropic hosts).
    Anthropic,
    /// The same proxy with a longer list. Entries: `anthropic` (the hosts
    /// above), a hostname (suffix match), or `localhost:<port>` for a
    /// server on this machine, which is reached directly.
    Allow(Vec<String>),
}

impl NetworkPolicy {
    pub fn restricted(&self) -> bool {
        *self != NetworkPolicy::Full
    }

    /// Hostnames the proxy tunnels to (suffix match).
    pub fn hosts(&self) -> Vec<String> {
        let anthropic = || ["anthropic.com", "claude.ai", "claude.com"].iter().map(|s| s.to_string());
        match self {
            NetworkPolicy::Full => vec![],
            NetworkPolicy::Anthropic => anthropic().collect(),
            NetworkPolicy::Allow(list) => {
                let mut out: Vec<String> = vec![];
                for e in list {
                    if e == "anthropic" {
                        out.extend(anthropic());
                    } else if Self::local_port(e).is_none() {
                        out.push(e.trim().trim_start_matches("*.").to_lowercase());
                    }
                }
                out
            }
        }
    }

    fn local_port(entry: &str) -> Option<u16> {
        let (host, port) = entry.trim().rsplit_once(':')?;
        matches!(host, "localhost" | "127.0.0.1").then(|| port.parse().ok()).flatten()
    }

    /// Ports on this machine the run may connect to directly.
    pub fn local_ports(&self) -> Vec<u16> {
        match self {
            NetworkPolicy::Allow(list) => list.iter().filter_map(|e| Self::local_port(e)).collect(),
            _ => vec![],
        }
    }
}

impl Serialize for NetworkPolicy {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            NetworkPolicy::Full => ser.serialize_str("full"),
            NetworkPolicy::Anthropic => ser.serialize_str("anthropic"),
            NetworkPolicy::Allow(list) => {
                let mut m = ser.serialize_map(Some(1))?;
                m.serialize_entry("allow", list)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for NetworkPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Table { allow: Vec<String> },
        }
        match Raw::deserialize(de)? {
            Raw::Name(n) if n == "full" => Ok(NetworkPolicy::Full),
            Raw::Name(n) if n == "anthropic" => Ok(NetworkPolicy::Anthropic),
            Raw::Name(n) => Err(serde::de::Error::custom(format!("unknown network policy `{n}` (use \"full\", \"anthropic\", or {{ allow = [...] }})"))),
            Raw::Table { allow } => Ok(NetworkPolicy::Allow(allow)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default = "default_runs")]
    pub runs_per_cell: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub network: NetworkPolicy,
    /// Metric used for verdicts, ablation attribution and advice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_metric: Option<String>,
    /// Optional spending cap for one launch, agent plus judge cost. When it
    /// is reached no further runs start; finished runs are kept and the
    /// launch can be resumed after raising the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// Keep run workspaces on disk after the run instead of deleting them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_workspaces: bool,
    #[serde(default)]
    pub runner: RunnerCfg,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<Factor>,
    #[serde(default)]
    pub tasks: TaskSel,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub graders: BTreeMap<String, GraderDefaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ablation: Option<AblationMeta>,
    /// Trigger experiment: which skill(s) the tasks' `trigger` fields are about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerCfg>,
}

/// `[trigger] skill = "name"` (or `skills = [...]`). Every task that carries
/// `trigger = "should" | "should_not"` is graded on whether the skill fired.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TriggerCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

impl TriggerCfg {
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.skill.iter().cloned().collect();
        for s in &self.skills {
            if !v.contains(s) {
                v.push(s.clone());
            }
        }
        v
    }
}

fn default_runs() -> u32 {
    DEFAULT_RUNS_PER_CELL
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn default_runner_kind() -> String {
    "claude-code".into()
}
fn default_concurrency() -> usize {
    3
}
fn default_retries() -> u32 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerCfg {
    #[serde(default = "default_runner_kind")]
    pub kind: String,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Default model when no `model` factor is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Default effort when no `effort` factor is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Plugins under test, passed with `--plugin-dir` (never installed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_dirs: Vec<String>,
    /// Rate-limit retries per run before the run is recorded as failed.
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    /// Codex only: sandbox mode passed to `codex exec --sandbox`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_sandbox: Option<String>,
    /// `isolation = "docker"`: image that has the runner CLI installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,
    /// Host environment variables passed into the container by name.
    /// Default: ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, OPENAI_API_KEY.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docker_env: Vec<String>,
    /// Extra arguments for `docker run` (mounts, `--network`, resource limits).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docker_args: Vec<String>,
}

impl Default for RunnerCfg {
    fn default() -> Self {
        RunnerCfg {
            kind: default_runner_kind(),
            concurrency: default_concurrency(),
            model: None,
            effort: None,
            max_turns: None,
            plugin_dirs: vec![],
            max_retries: default_retries(),
            codex_sandbox: None,
            docker_image: None,
            docker_env: vec![],
            docker_args: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorKind {
    Overlay,
    Model,
    Effort,
    Runner,
    /// Text appended to the system prompt. A level starting with `file:` is
    /// read from that path; an empty level means no append.
    #[serde(alias = "append", alias = "system_prompt")]
    SystemPromptAppend,
    /// Arbitrary runner knob: a CLI flag (`flag`) or environment variable (`env`).
    Knob,
    /// MCP servers: each level is an MCP config file (`{"mcpServers": ...}`)
    /// relative to the project; `""` means none.
    Mcp,
}

impl FactorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FactorKind::Overlay => "overlay",
            FactorKind::Model => "model",
            FactorKind::Effort => "effort",
            FactorKind::Runner => "runner",
            FactorKind::SystemPromptAppend => "system_prompt_append",
            FactorKind::Knob => "knob",
            FactorKind::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Factor {
    pub name: String,
    pub kind: FactorKind,
    pub levels: Vec<Level>,
    /// Knob factors: CLI flag the level value is passed to (e.g. `--max-turns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    /// Knob factors: environment variable the level value is assigned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

/// A factor level. Written as a bare string, or a table with a `label` and
/// extra keys. Runner levels may carry `model` / `effort` so each runner in a
/// cross-runner experiment gets a model it actually supports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Level {
    Plain(String),
    Detailed {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(flatten)]
        extra: BTreeMap<String, String>,
    },
}

impl Level {
    pub fn value(&self) -> &str {
        match self {
            Level::Plain(s) => s,
            Level::Detailed { value, .. } => value,
        }
    }
    pub fn label(&self) -> String {
        match self {
            Level::Plain(s) => short_label(s),
            Level::Detailed { value, label, .. } => label.clone().unwrap_or_else(|| short_label(value)),
        }
    }
    pub fn extra(&self, key: &str) -> Option<&str> {
        match self {
            Level::Plain(_) => None,
            Level::Detailed { extra, .. } => extra.get(key).map(|s| s.as_str()),
        }
    }
}

fn short_label(v: &str) -> String {
    if v.is_empty() {
        return "(none)".into();
    }
    let one_line = v.lines().next().unwrap_or("");
    let base = if !v.contains(' ') && v.contains('/') {
        v.trim_end_matches('/').rsplit('/').next().unwrap_or(v)
    } else {
        one_line
    };
    let mut s: String = base.chars().take(40).collect();
    if base.chars().count() > 40 {
        s.push('…');
    }
    s
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskSel {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraderDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human: Option<HumanCfg>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deterministic: Vec<DetSpec>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeCfg {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Markdown rubric, relative to the project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
    /// Score each run against expectations and the rubric.
    #[serde(default = "default_true")]
    pub rubric_scoring: bool,
    /// Blind A/B between runs of the same task, both orders.
    #[serde(default)]
    pub pairwise: bool,
    /// `baseline`: every variant against the first. `all`: every variant pair.
    /// Default: `all` up to 4 variants, `baseline` above that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairwise_mode: Option<String>,
    /// Repetitions paired per task and variant pair. Default min(runs_per_cell, 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairwise_reps: Option<u32>,
    /// `tools`: also show the judge a compact trace of the agent's tool
    /// calls (name and one-line input, no results). Default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

impl JudgeCfg {
    pub fn shows_tools(&self) -> bool {
        self.transcript.as_deref() == Some("tools")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanCfg {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AblationMeta {
    /// File the sections were split from, relative to the project root.
    pub source: String,
    /// `skill` or `claude_md`.
    pub kind: String,
    pub depth: u32,
    /// Factor that carries the ablation overlays.
    pub factor: String,
    pub baseline: String,
    pub full: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_only: Option<String>,
    /// `sections` (default): the body split at headings.
    /// `description_sentences`: the frontmatter description split into
    /// sentences, body intact; `sections` then holds one entry per sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Overlay level holding the placebo: same frontmatter, body replaced by
    /// innocuous text of matched length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placebo: Option<String>,
    pub sections: Vec<AblationSection>,
}

impl AblationMeta {
    pub fn by_sentence(&self) -> bool {
        self.mode.as_deref() == Some("description_sentences")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AblationSection {
    pub index: usize,
    pub title: String,
    /// Overlay level value with this section removed.
    pub overlay: String,
    pub est_tokens: u64,
    pub lines: (usize, usize),
}

// ---------------------------------------------------------------- tasks

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub prompt: String,
    /// Read the prompt from this file instead (relative to the project root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<Fixture>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expected_output: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expectations: Vec<String>,
    /// A result someone accepted for this task (a harvested run's output).
    /// The rubric judge is shown it as a reference point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Workspace file that is "the output" for judges and reviewers. Without
    /// it the agent's final message is the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Stop the agent after this many turns (overrides `[runner] max_turns`).
    /// Reaching a task's own cap is a normal end, not a timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// `should` or `should_not`: whether the skill named by the experiment's
    /// `[trigger]` ought to fire on this prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Multi-turn task: a simulated user answers the agent and decides when
    /// the task is done. `prompt` is the opening message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Conversation>,
    #[serde(default, skip_serializing_if = "TaskGraders::is_empty")]
    pub graders: TaskGraders,
    /// Path of the task file relative to the project root (set at load).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

fn default_user_turns() -> u32 {
    6
}

/// The simulated user of a conversation task: a second model that plays
/// the person. It holds a persona and information the agent does not have,
/// answers the agent's questions, and decides when the task is done.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Who the user is and how they answer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub persona: String,
    /// What the user knows and reveals only when asked.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hidden: String,
    /// Replies the simulated user may send before the run is stopped.
    #[serde(default = "default_user_turns")]
    pub max_user_turns: u32,
    /// Simulator model. Default: the experiment's judge model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// When the user considers the task finished.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub done_when: String,
    /// Show the simulator the names of the tools the agent called (someone
    /// watching a terminal). `false` is a chat user who sees only messages.
    #[serde(default = "default_true")]
    pub sees_tools: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskGraders {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deterministic: Vec<DetSpec>,
    /// Override the experiment default: `false` skips the LLM judge for this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    /// Directory copied into the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Git repository (path or URL) cloned into the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub git_ref: Option<String>,
}

/// A tool name, or several that count as the same tool across runners
/// (`["Bash", "shell"]`). A trailing `*` matches a prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Names {
    One(String),
    Many(Vec<String>),
}

impl Names {
    pub fn list(&self) -> Vec<&str> {
        match self {
            Names::One(s) => vec![s.as_str()],
            Names::Many(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
    pub fn matches(&self, tool: &str) -> bool {
        self.list().iter().any(|p| crate::transcript::tool_name_matches(p, tool))
    }
    pub fn display(&self) -> String {
        self.list().join("|")
    }
}

/// One step of a `sequence` grader: a tool call by name, optionally with a
/// regex over its JSON-serialized input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeqStep {
    pub name: Names,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

fn default_one() -> Option<u64> {
    Some(1)
}
fn default_include() -> String {
    "include".into()
}
fn default_assistant() -> String {
    "assistant".into()
}

impl TaskGraders {
    pub fn is_empty(&self) -> bool {
        self.deterministic.is_empty() && self.judge.is_none() && self.human.is_none()
    }
}

/// Deterministic grader specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetSpec {
    /// Shell command run in the workspace; passes on the expected exit code.
    Command {
        cmd: String,
        #[serde(default)]
        expect_exit: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    FileExists {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Regex over a workspace file, or over the output when `path` is absent.
    Regex {
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default = "default_true")]
        should_match: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Bounds on changed lines (added + removed) in the workspace diff.
    DiffSize {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    WordCount {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Validate a JSON file (or the output) against a schema file or inline schema.
    JsonSchema {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema_file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Passes when a skill was (or with `expect = false`, was not) invoked.
    SkillInvoked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill: Option<String>,
        #[serde(default = "default_true")]
        expect: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Passes when the number of matching tool calls is within bounds.
    /// `name` is the tool here, so this grader's own name is `label`.
    ToolCall {
        #[serde(alias = "tool")]
        name: Names,
        /// Regex over the JSON-serialized input.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default = "default_one", skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        /// include | exclude | only
        #[serde(default = "default_include")]
        subagents: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Passes when each step matches a tool call, in order, each after the
    /// previous; with `before_final`, all before the last assistant message.
    Sequence {
        steps: Vec<SeqStep>,
        #[serde(default = "default_true")]
        before_final: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Regex over the transcript text: assistant | tool_results | thinking | all.
    TranscriptRegex {
        pattern: String,
        #[serde(default = "default_assistant")]
        scope: String,
        #[serde(default = "default_true")]
        should_match: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Passes when the count of hook executions for an event is within bounds.
    HookFired {
        event: String,
        #[serde(default = "default_one", skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl DetSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            DetSpec::Command { .. } => "command",
            DetSpec::FileExists { .. } => "file_exists",
            DetSpec::Regex { .. } => "regex",
            DetSpec::DiffSize { .. } => "diff_size",
            DetSpec::WordCount { .. } => "word_count",
            DetSpec::JsonSchema { .. } => "json_schema",
            DetSpec::SkillInvoked { .. } => "skill_invoked",
            DetSpec::ToolCall { .. } => "tool_call",
            DetSpec::Sequence { .. } => "sequence",
            DetSpec::TranscriptRegex { .. } => "transcript_regex",
            DetSpec::HookFired { .. } => "hook_fired",
        }
    }
    pub fn name(&self) -> Option<&str> {
        match self {
            DetSpec::Command { name, .. }
            | DetSpec::FileExists { name, .. }
            | DetSpec::Regex { name, .. }
            | DetSpec::DiffSize { name, .. }
            | DetSpec::WordCount { name, .. }
            | DetSpec::JsonSchema { name, .. }
            | DetSpec::SkillInvoked { name, .. }
            | DetSpec::Sequence { name, .. }
            | DetSpec::TranscriptRegex { name, .. }
            | DetSpec::HookFired { name, .. } => name.as_deref(),
            DetSpec::ToolCall { label, .. } => label.as_deref(),
        }
    }
}

// ---------------------------------------------------------------- variants

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelRef {
    pub kind: FactorKind,
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

/// One combination of factor levels, with runner defaults resolved in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    /// Stable identity: `factor=label` pairs in factor order joined by `|`.
    pub key: String,
    pub label: String,
    /// Factor name → chosen level, in experiment factor order.
    pub levels: Vec<(String, LevelRef)>,
    pub runner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Skills this variant's overlays install, with their descriptions as
    /// they were when the launch was created. Empty until then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// Directory name under `.claude/skills/`.
    pub dir: String,
    /// `name` from the frontmatter, or the directory name.
    pub name: String,
    pub description: String,
}

impl Variant {
    pub fn level(&self, factor: &str) -> Option<&LevelRef> {
        self.levels.iter().find(|(n, _)| n == factor).map(|(_, l)| l)
    }
    pub fn level_label(&self, factor: &str) -> Option<&str> {
        self.level(factor).map(|l| l.label.as_str())
    }
    /// Overlay directories (project-relative), in factor order. `none` and
    /// the empty string are the empty baseline and are skipped.
    pub fn overlays(&self) -> Vec<&str> {
        self.levels
            .iter()
            .filter(|(_, l)| l.kind == FactorKind::Overlay && !is_empty_overlay(&l.value))
            .map(|(_, l)| l.value.as_str())
            .collect()
    }
    pub fn appends(&self) -> Vec<&str> {
        self.levels
            .iter()
            .filter(|(_, l)| l.kind == FactorKind::SystemPromptAppend && !l.value.is_empty())
            .map(|(_, l)| l.value.as_str())
            .collect()
    }
    /// MCP config files (project-relative), in factor order. Several `mcp`
    /// factors stack: every non-empty level contributes its servers.
    pub fn mcp_configs(&self) -> Vec<&str> {
        self.levels.iter().filter(|(_, l)| l.kind == FactorKind::Mcp && !l.value.trim().is_empty() && l.value != "none").map(|(_, l)| l.value.as_str()).collect()
    }
    pub fn knobs(&self) -> Vec<&LevelRef> {
        self.levels.iter().filter(|(_, l)| l.kind == FactorKind::Knob).map(|(_, l)| l).collect()
    }
}

pub fn is_empty_overlay(value: &str) -> bool {
    let v = value.trim_end_matches('/');
    v.is_empty() || v == "none" || v.ends_with("/none")
}

impl Experiment {
    pub fn parse(text: &str) -> Result<Experiment> {
        let exp: Experiment = toml::from_str(text).context("parsing experiment TOML")?;
        exp.check()?;
        Ok(exp)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn check(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("experiment name is empty");
        }
        if self.runs_per_cell == 0 {
            bail!("runs_per_cell must be at least 1");
        }
        if self.budget_usd.map(|b| !(b > 0.0)).unwrap_or(false) {
            bail!("budget_usd must be greater than zero");
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.factors {
            if !seen.insert(f.name.as_str()) {
                bail!("duplicate factor name `{}`", f.name);
            }
            if f.levels.is_empty() {
                bail!("factor `{}` has no levels", f.name);
            }
            let mut labels = std::collections::BTreeSet::new();
            for l in &f.levels {
                if !labels.insert(l.label()) {
                    bail!("factor `{}` has two levels labelled `{}`; give one a distinct `label`", f.name, l.label());
                }
            }
            if f.kind == FactorKind::Knob && f.flag.is_none() && f.env.is_none() {
                bail!("knob factor `{}` needs `flag` or `env`", f.name);
            }
        }
        for kind in [FactorKind::Model, FactorKind::Effort, FactorKind::Runner] {
            if self.factors.iter().filter(|f| f.kind == kind).count() > 1 {
                bail!("at most one `{}` factor is allowed", kind.as_str());
            }
        }
        Ok(())
    }

    pub fn default_graders(&self) -> GraderDefaults {
        self.graders.get("default").cloned().unwrap_or_default()
    }

    pub fn judge(&self) -> Option<JudgeCfg> {
        self.default_graders().judge
    }

    pub fn human_enabled(&self) -> bool {
        self.default_graders().human.map(|h| h.enabled).unwrap_or(false)
    }

    /// Cross product of all factor levels. An experiment without factors has
    /// one variant: the runner defaults.
    pub fn variants(&self) -> Vec<Variant> {
        let mut combos: Vec<Vec<(String, LevelRef)>> = vec![vec![]];
        for f in &self.factors {
            let mut next = Vec::with_capacity(combos.len() * f.levels.len());
            for c in &combos {
                for l in &f.levels {
                    let mut c2 = c.clone();
                    let extra = match l {
                        Level::Plain(_) => BTreeMap::new(),
                        Level::Detailed { extra, .. } => extra.clone(),
                    };
                    c2.push((
                        f.name.clone(),
                        LevelRef {
                            kind: f.kind,
                            value: l.value().to_string(),
                            label: l.label(),
                            extra,
                            flag: f.flag.clone(),
                            env: f.env.clone(),
                        },
                    ));
                    next.push(c2);
                }
            }
            combos = next;
        }
        combos
            .into_iter()
            .map(|levels| {
                let find = |k: FactorKind| levels.iter().find(|(_, l)| l.kind == k).map(|(_, l)| l);
                let runner_level = find(FactorKind::Runner);
                let runner = runner_level.map(|l| l.value.clone()).unwrap_or_else(|| self.runner.kind.clone());
                let from_runner = |key: &str| runner_level.and_then(|l| l.extra.get(key).cloned());
                let model = find(FactorKind::Model)
                    .map(|l| l.value.clone())
                    .or_else(|| from_runner("model"))
                    .or_else(|| if runner == self.runner.kind { self.runner.model.clone() } else { None });
                let effort = find(FactorKind::Effort)
                    .map(|l| l.value.clone())
                    .or_else(|| from_runner("effort"))
                    .or_else(|| if runner == self.runner.kind { self.runner.effort.clone() } else { None });
                let key = if levels.is_empty() {
                    "default".to_string()
                } else {
                    levels.iter().map(|(n, l)| format!("{}={}", n, l.label)).collect::<Vec<_>>().join("|")
                };
                let label = if levels.is_empty() {
                    "default".to_string()
                } else {
                    levels.iter().map(|(_, l)| l.label.clone()).collect::<Vec<_>>().join(" · ")
                };
                Variant { key, label, levels, runner, model, effort, skills: vec![] }
            })
            .collect()
    }
}

impl Task {
    /// `Some(true)` when the skill should fire on this prompt, `Some(false)`
    /// when it should not, `None` for an ordinary task.
    pub fn should_trigger(&self) -> Option<bool> {
        self.trigger.as_deref().map(|t| t == "should")
    }

    pub fn parse(text: &str) -> Result<Task> {
        let t: Task = toml::from_str(text).context("parsing task TOML")?;
        if t.id.trim().is_empty() {
            bail!("task id is empty");
        }
        if t.prompt.trim().is_empty() && t.prompt_file.is_none() {
            bail!("task `{}` has neither `prompt` nor `prompt_file`", t.id);
        }
        if let Some(c) = &t.conversation {
            if c.max_user_turns == 0 {
                bail!("task `{}`: conversation.max_user_turns must be at least 1", t.id);
            }
        }
        if let Some(tr) = &t.trigger {
            if tr != "should" && tr != "should_not" {
                bail!("task `{}`: trigger must be \"should\" or \"should_not\", not \"{tr}\"", t.id);
            }
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKETCH: &str = r#"
name = "reader-first-writing-effort"
description = "Does rfw help on Opus 5.5, and does effort matter?"
runs_per_cell = 5
timeout_secs = 900
isolation = "seatbelt"

[runner]
kind = "claude-code"
concurrency = 3

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none", "overlays/rfw-v1.1.0", "overlays/rfw-v1.2-draft"]

[[factors]]
name = "model"
kind = "model"
levels = ["claude-opus-5-5", "claude-sonnet-5-5"]

[[factors]]
name = "effort"
kind = "effort"
levels = ["medium", "high"]

[tasks]
include = ["tasks/writing/*.toml"]

[graders.default]
judge = { model = "claude-opus-5-5", rubric = "rubrics/reader-first.md", pairwise = true }
human = { enabled = true }
"#;

    #[test]
    fn parses_design_sketch() {
        let e = Experiment::parse(SKETCH).unwrap();
        assert_eq!(e.isolation, Isolation::Seatbelt);
        let vs = e.variants();
        assert_eq!(vs.len(), 12);
        assert_eq!(vs[0].key, "overlay=none|model=claude-opus-5-5|effort=medium");
        assert!(vs[0].overlays().is_empty());
        assert_eq!(vs[4].overlays(), vec!["overlays/rfw-v1.1.0"]);
        assert_eq!(vs[1].effort.as_deref(), Some("high"));
        assert!(e.judge().unwrap().pairwise);
        assert!(e.human_enabled());
        // Round-trips through TOML.
        let again = Experiment::parse(&e.to_toml().unwrap()).unwrap();
        assert_eq!(again.variants().len(), 12);
    }

    #[test]
    fn parses_task_sketch() {
        let t = Task::parse(
            r#"
id = "incident-summary"
prompt = "Summarize the attached incident report for the on-call channel."
fixture = { dir = "fixtures/incident-report" }
expected_output = "A short, plain summary under 150 words leading with impact and current status."
expectations = ["Leads with user impact", "Under 150 words", "No hedging phrases"]
tags = ["writing", "summary"]

[graders]
deterministic = [{ kind = "word_count", path = "SUMMARY.md", max = 150 }]
"#,
        )
        .unwrap();
        assert_eq!(t.expectations.len(), 3);
        assert!(matches!(t.graders.deterministic[0], DetSpec::WordCount { max: Some(150), .. }));
    }

    #[test]
    fn runner_levels_carry_models() {
        let e = Experiment::parse(
            r#"
name = "x"
[[factors]]
name = "runner"
kind = "runner"
levels = [{ value = "claude-code", model = "claude-haiku-5-5" }, { value = "codex", model = "gpt-5.5", effort = "low" }]
"#,
        )
        .unwrap();
        let vs = e.variants();
        assert_eq!(vs[1].runner, "codex");
        assert_eq!(vs[1].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(vs[1].effort.as_deref(), Some("low"));
    }

    #[test]
    fn network_policy_forms() {
        let parse = |v: &str| Experiment::parse(&format!("name = 'x'\nnetwork = {v}\n")).unwrap().network;
        assert_eq!(parse("'full'"), NetworkPolicy::Full);
        assert_eq!(parse("'anthropic'"), NetworkPolicy::Anthropic);
        let allow = parse("{ allow = ['anthropic', 'localhost:8080', '127.0.0.1:9000', 'registry.npmjs.org'] }");
        assert_eq!(allow.local_ports(), vec![8080, 9000]);
        assert_eq!(allow.hosts(), vec!["anthropic.com", "claude.ai", "claude.com", "registry.npmjs.org"]);
        assert!(allow.restricted() && !NetworkPolicy::Full.restricted());
        assert!(Experiment::parse("name = 'x'\nnetwork = 'open'\n").is_err());
        // Round-trips through TOML and through the JSON a launch is stored as.
        let exp = Experiment::parse("name = 'x'\nnetwork = { allow = ['anthropic', 'localhost:8080'] }\n").unwrap();
        assert_eq!(Experiment::parse(&exp.to_toml().unwrap()).unwrap().network, exp.network);
        assert_eq!(serde_json::from_str::<Experiment>(&serde_json::to_string(&exp).unwrap()).unwrap().network, exp.network);
    }

    #[test]
    fn mcp_levels_stack() {
        let e = Experiment::parse(
            "name = 'x'\n[[factors]]\nname = 'fs'\nkind = 'mcp'\nlevels = ['', { value = 'mcp/filesystem.json', label = 'fs' }]\n[[factors]]\nname = 'search'\nkind = 'mcp'\nlevels = ['mcp/search.json']\n",
        )
        .unwrap();
        let vs = e.variants();
        assert_eq!(vs[0].mcp_configs(), vec!["mcp/search.json"]);
        assert_eq!(vs[1].mcp_configs(), vec!["mcp/filesystem.json", "mcp/search.json"]);
        assert_eq!(vs[1].label, "fs · search.json");
    }

    #[test]
    fn rejects_duplicate_labels() {
        let r = Experiment::parse(
            "name='x'\n[[factors]]\nname='o'\nkind='overlay'\nlevels=['a/x','b/x']\n",
        );
        assert!(r.is_err());
    }
}

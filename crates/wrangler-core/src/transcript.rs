//! The normalized transcript shape shared by every runner: a list of turns
//! with role, text, tool calls and tool results. Runner-specific output is
//! kept alongside as the raw artifact.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Turn {
    /// assistant | user | system
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Visible reasoning, when the runner exposes it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    /// True for turns produced inside a subagent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
    /// True for user turns written by the simulated user in a conversation task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub simulated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// A tool call with where it sits in the transcript.
#[derive(Debug, Clone, Copy)]
pub struct CallAt<'a> {
    /// Index of the turn that made the call.
    pub turn: usize,
    pub call: &'a ToolCall,
    pub subagent: bool,
}

/// Runner-neutral tool name: Claude Code's `mcp__server__tool` becomes
/// `mcp.server.tool`, the form Codex already reports.
pub fn normalize_tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => format!("mcp.{server}.{tool}"),
        _ => name.to_string(),
    }
}

/// Does a tool name match a pattern? Exact, or a prefix when the pattern
/// ends in `*` (`mcp.filesystem.*`).
pub fn tool_name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// One line describing a call's input: the command, path or query when
/// there is one, else the compact JSON.
pub fn input_summary(input: &Value, max: usize) -> String {
    let pick = ["command", "file_path", "path", "pattern", "query", "url", "skill", "description", "prompt"].iter().find_map(|k| input.get(*k).and_then(|v| v.as_str()));
    let text = match pick {
        Some(s) => s.to_string(),
        None if input.is_null() => String::new(),
        None => input.to_string(),
    };
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > max {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    } else {
        one_line
    }
}

impl Transcript {
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.turns.iter().flat_map(|t| t.tool_calls.iter())
    }

    /// Every tool call in order, with its turn index, so a grader knows
    /// where it falls relative to the final message.
    pub fn tool_calls_with_turn(&self) -> Vec<CallAt<'_>> {
        self.turns.iter().enumerate().flat_map(|(i, t)| t.tool_calls.iter().map(move |c| CallAt { turn: i, call: c, subagent: t.subagent })).collect()
    }

    /// Index of the turn holding the final message: the last assistant
    /// turn with text outside subagents.
    pub fn final_turn(&self) -> Option<usize> {
        self.turns.iter().rposition(|t| t.role == "assistant" && !t.subagent && !t.text.trim().is_empty())
    }

    /// Tool calls made before the final message (all of them when there is none).
    pub fn calls_before_final(&self) -> usize {
        let end = self.final_turn().unwrap_or(self.turns.len());
        self.tool_calls_with_turn().iter().filter(|c| c.turn < end).count()
    }

    /// Compact process trace for judges: one line per tool call, no results.
    pub fn tool_trace(&self, max_calls: usize) -> String {
        let calls = self.tool_calls_with_turn();
        let mut s = String::new();
        for (i, c) in calls.iter().take(max_calls).enumerate() {
            let sub = if c.subagent { " (subagent)" } else { "" };
            s.push_str(&format!("{}. {}{sub}: {}\n", i + 1, c.call.name, input_summary(&c.call.input, 160)));
        }
        if calls.len() > max_calls {
            s.push_str(&format!("[... {} more tool calls ...]\n", calls.len() - max_calls));
        }
        s
    }

    /// The dialog of a conversation run as text: what each side said, in
    /// order, without tool calls or results. The agent's side is the
    /// messages it wrote outside subagents.
    pub fn dialog_text(&self) -> String {
        let mut s = String::new();
        for t in self.turns.iter().filter(|t| !t.subagent && !t.text.trim().is_empty() && t.tool_results.is_empty()) {
            let who = match (t.role.as_str(), t.simulated) {
                ("user", true) => "user (simulated)",
                ("user", false) => "user",
                _ => "agent",
            };
            s.push_str(&format!("[{who}] {}\n\n", t.text.trim()));
        }
        s
    }

    /// Simulated-user turns in a conversation run.
    pub fn user_turns(&self) -> usize {
        self.turns.iter().filter(|t| t.role == "user" && t.simulated).count()
    }

    /// How often the agent asked the user something: assistant turns
    /// (outside subagents) whose text ends in a question, plus
    /// `AskUserQuestion` tool calls.
    pub fn questions_asked(&self) -> usize {
        self.turns
            .iter()
            .filter(|t| t.role == "assistant" && !t.subagent)
            .map(|t| {
                let asks = t.tool_calls.iter().filter(|c| c.name == "AskUserQuestion").count();
                let ends = t.text.trim_end().trim_end_matches(|c: char| matches!(c, '*' | '_' | '`' | ')' | '"' | '\'')).ends_with('?');
                asks + usize::from(ends && asks == 0 && t.tool_calls.is_empty())
            })
            .sum()
    }

    /// Last non-empty assistant text outside subagents.
    pub fn final_text(&self) -> String {
        self.turns
            .iter()
            .rev()
            .find(|t| t.role == "assistant" && !t.subagent && !t.text.trim().is_empty())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    /// Names of skills the agent invoked: a `Skill` tool call, or a read of
    /// a `SKILL.md` (by a file tool or a shell command).
    pub fn skills_invoked(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut add = |s: String| {
            if !s.is_empty() && !out.contains(&s) {
                out.push(s)
            }
        };
        for c in self.tool_calls() {
            if c.name == "Skill" {
                if let Some(s) = c.input.get("skill").and_then(|v| v.as_str()) {
                    add(s.rsplit(':').next().unwrap_or(s).to_string());
                }
                continue;
            }
            let hay = match c.input.get("file_path").or_else(|| c.input.get("command")).or_else(|| c.input.get("path")) {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => continue,
            };
            let mut rest = hay.as_str();
            while let Some(i) = rest.find("/SKILL.md") {
                let before = &rest[..i];
                let name = before.rsplit(|ch: char| ch == '/' || ch.is_whitespace() || ch == '"' || ch == '\'').next().unwrap_or("");
                add(name.to_string());
                rest = &rest[i + 9..];
            }
        }
        out
    }

    /// Plain-text rendering for the CLI and for judges that need the path.
    pub fn render(&self, max_result_chars: usize) -> String {
        let mut s = String::new();
        for t in &self.turns {
            let who = if t.subagent { format!("{} (subagent)", t.role) } else if t.simulated { "user (simulated)".to_string() } else { t.role.clone() };
            if !t.text.trim().is_empty() {
                s.push_str(&format!("[{who}]\n{}\n\n", t.text.trim()));
            }
            for c in &t.tool_calls {
                let input = crate::util::truncate_middle(&c.input.to_string(), max_result_chars);
                s.push_str(&format!("[{who} → {}] {input}\n\n", c.name));
            }
            for r in &t.tool_results {
                let tag = if r.is_error { "tool error" } else { "tool result" };
                s.push_str(&format!("[{tag}]\n{}\n\n", crate::util::truncate_middle(r.content.trim(), max_result_chars)));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_skills() {
        let t = Transcript {
            turns: vec![Turn {
                role: "assistant".into(),
                tool_calls: vec![
                    ToolCall { id: "1".into(), name: "Skill".into(), input: json!({"skill": "plugin:pirate"}) },
                    ToolCall { id: "2".into(), name: "Read".into(), input: json!({"file_path": "/ws/.claude/skills/rfw/SKILL.md"}) },
                    ToolCall { id: "3".into(), name: "shell".into(), input: json!({"command": "cat /h/skills/other/SKILL.md"}) },
                ],
                ..Default::default()
            }],
        };
        assert_eq!(t.skills_invoked(), vec!["pirate", "rfw", "other"]);
    }

    #[test]
    fn names_order_and_questions() {
        assert_eq!(normalize_tool_name("mcp__filesystem__read_file"), "mcp.filesystem.read_file");
        assert_eq!(normalize_tool_name("mcp__a__b__c"), "mcp.a.b__c");
        assert_eq!(normalize_tool_name("Bash"), "Bash");
        assert!(tool_name_matches("mcp.fs.*", "mcp.fs.read") && !tool_name_matches("mcp.fs.*", "mcp.other.read") && tool_name_matches("Bash", "Bash"));
        let call = |id: &str, name: &str| ToolCall { id: id.into(), name: name.into(), input: json!({"command": "ls  -la\n/tmp"}) };
        let t = Transcript {
            turns: vec![
                Turn { role: "assistant".into(), text: "Which test fails?".into(), ..Default::default() },
                Turn { role: "user".into(), text: "The unicode one.".into(), simulated: true, ..Default::default() },
                Turn { role: "assistant".into(), tool_calls: vec![call("1", "Bash")], ..Default::default() },
                Turn { role: "assistant".into(), text: "Fixed.".into(), ..Default::default() },
                Turn { role: "assistant".into(), tool_calls: vec![call("2", "Bash")], ..Default::default() },
            ],
        };
        assert_eq!(t.final_turn(), Some(3));
        assert_eq!(t.calls_before_final(), 1);
        assert_eq!((t.user_turns(), t.questions_asked()), (1, 1));
        assert_eq!(t.tool_trace(1), "1. Bash: ls -la /tmp\n[... 1 more tool calls ...]\n");
    }
}

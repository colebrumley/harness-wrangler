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

impl Transcript {
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.turns.iter().flat_map(|t| t.tool_calls.iter())
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
            let who = if t.subagent { format!("{} (subagent)", t.role) } else { t.role.clone() };
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
}

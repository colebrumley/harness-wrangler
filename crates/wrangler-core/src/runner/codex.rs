//! Codex CLI runner.
//!
//! Isolation: each run gets its own `CODEX_HOME` holding only a symlink to
//! the user's `auth.json`, so user-level `AGENTS.md`, config, skills and
//! history stay out. Overlays written for Claude Code are translated:
//! `CLAUDE.md` becomes `AGENTS.md`, and `.claude/skills/*` are installed as
//! Codex skills.

use super::*;
use crate::project::resolve_append;
use crate::transcript::{ToolCall, ToolResult, Turn};
use crate::workspace::copy_dir_contents;
use serde_json::{json, Value};

#[derive(Default)]
pub struct CodexRunner {
    pub binary: Option<String>,
}

impl CodexRunner {
    fn binary(&self) -> String {
        self.binary.clone().or_else(|| std::env::var("WRANGLER_CODEX_BIN").ok()).unwrap_or_else(|| "codex".into())
    }

    fn user_home() -> PathBuf {
        std::env::var("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".codex"))
    }

    pub fn build_args(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Vec<String> {
        // Codex confines model-run commands with its own seatbelt sandbox, so
        // that is the isolation primitive here at every ladder level.
        let sandbox = limits.cfg.codex_sandbox.clone().unwrap_or_else(|| "workspace-write".into());
        let mut a: Vec<String> = vec!["exec".into(), "--json".into(), "--sandbox".into(), sandbox, "--skip-git-repo-check".into(), "--ephemeral".into()];
        a.extend(["-C".into(), ws.dir.to_string_lossy().to_string()]);
        if let Some(m) = &variant.model {
            a.extend(["-m".into(), m.clone()]);
        }
        if let Some(e) = &variant.effort {
            a.extend(["-c".into(), format!("model_reasoning_effort=\"{e}\"")]);
        }
        for k in variant.knobs() {
            if let Some(flag) = &k.flag {
                a.push(flag.clone());
                if !k.value.is_empty() {
                    a.push(k.value.clone());
                }
            }
        }
        a.push("--".into());
        a.push(task.prompt.clone());
        a
    }
}

impl AgentRunner for CodexRunner {
    fn id(&self) -> &str {
        "codex"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            overlay: Support::Approximate("CLAUDE.md is installed as AGENTS.md and .claude/skills as Codex skills; .claude/settings.json, hooks and agents are ignored"),
            model: Support::Full,
            effort: Support::Full,
            system_prompt_append: Support::Approximate("appended to the workspace AGENTS.md, not the system prompt"),
            knob: Support::Full,
            seatbelt: Support::Approximate("Codex applies its own workspace-write seatbelt sandbox; the network policy setting is not applied"),
            reports_cost: false,
        }
    }

    fn materialize(&self, ws: &Workspace, variant: &Variant) -> Result<()> {
        for o in variant.overlays() {
            ws.apply_overlay(o)?;
        }
        let agents = ws.dir.join("AGENTS.md");
        let claude_md = ws.dir.join("CLAUDE.md");
        let has_overlay_claude_md = variant.overlays().iter().any(|o| ws.project.abs(o).join("CLAUDE.md").exists());
        if has_overlay_claude_md && claude_md.exists() {
            let mut text = std::fs::read_to_string(&agents).unwrap_or_default();
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&std::fs::read_to_string(&claude_md)?);
            std::fs::write(&agents, text)?;
        }
        let skills = ws.dir.join(".claude/skills");
        if skills.is_dir() {
            copy_dir_contents(&skills, &ws.dir.join(".agents/skills"))?;
        }
        let appends: Vec<String> = variant.appends().iter().map(|v| resolve_append(&ws.project, v)).collect::<Result<_>>()?;
        if !appends.is_empty() {
            let mut text = std::fs::read_to_string(&agents).unwrap_or_default();
            for a in appends {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&a);
            }
            std::fs::write(&agents, text)?;
        }
        Ok(())
    }

    fn run(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Result<RunRecord> {
        let home = ws.run_dir.join("codex-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home)?;
        let auth = Self::user_home().join("auth.json");
        if auth.exists() {
            std::os::unix::fs::symlink(&auth, home.join("auth.json"))?;
        }
        let skills = ws.dir.join(".claude/skills");
        if skills.is_dir() {
            copy_dir_contents(&skills, &home.join("skills"))?;
        }
        let args = self.build_args(ws, task, variant, limits);
        let mut env: Vec<(String, String)> = vec![("CODEX_HOME".into(), home.to_string_lossy().to_string())];
        env.extend(variant.knobs().iter().filter_map(|k| k.env.clone().map(|e| (e, k.value.clone()))));
        let spec = ExecSpec {
            program: self.binary(),
            args,
            cwd: ws.dir.clone(),
            env,
            env_remove: vec![],
            stdout_path: ws.run_dir.join("raw.jsonl"),
            stderr_path: ws.run_dir.join("stderr.txt"),
            timeout: limits.timeout,
            cancel: limits.cancel.clone(),
        };
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.iter().cloned());
        let out = exec(&spec)?;
        // The per-run home holds caches and state databases; only auth mattered.
        let _ = std::fs::remove_dir_all(&home);
        let mut rec = parse_jsonl(&out.stdout);
        rec.command = command;
        rec.model = variant.model.clone().unwrap_or_default();
        rec.raw_output = out.stdout;
        rec.stderr = out.stderr;
        rec.exit_status = out.exit;
        rec.timed_out = out.timed_out;
        rec.cancelled = out.cancelled;
        rec.wall_ms = out.wall_ms;
        rec.metrics.insert("wall_time_s".into(), out.wall_ms as f64 / 1000.0);
        if !rec.timed_out && !rec.cancelled && out.exit != Some(0) && rec.error.is_none() {
            let tail: String = rec.stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            rec.error = Some(format!("codex exited with {:?}. {tail}", out.exit));
        }
        if let Some(e) = &rec.error {
            if looks_rate_limited(e) {
                rec.rate_limited = Some(e.clone());
            }
        }
        Ok(rec)
    }
}

/// Normalize `codex exec --json` events into the common transcript shape.
/// Shell commands become `shell` tool calls and file changes `apply_patch`.
pub fn parse_jsonl(raw: &str) -> RunRecord {
    let mut rec = RunRecord::default();
    let mut turns = 0.0;
    let mut usage: BTreeMap<&str, f64> = BTreeMap::new();
    for line in raw.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else { continue };
        match ev["type"].as_str().unwrap_or("") {
            "item.completed" => {
                let it = &ev["item"];
                let id = it["id"].as_str().unwrap_or("").to_string();
                let mut turn = Turn { role: "assistant".into(), ..Default::default() };
                let mut result = None;
                match it["type"].as_str().unwrap_or("") {
                    "agent_message" => turn.text = it["text"].as_str().unwrap_or("").into(),
                    "reasoning" => turn.thinking = it["text"].as_str().unwrap_or("").into(),
                    "command_execution" => {
                        turn.tool_calls.push(ToolCall { id: id.clone(), name: "shell".into(), input: json!({"command": it["command"]}) });
                        let code = it["exit_code"].as_i64();
                        result = Some(ToolResult {
                            tool_use_id: id,
                            content: it["aggregated_output"].as_str().unwrap_or("").into(),
                            is_error: code.map(|c| c != 0).unwrap_or(false),
                        });
                    }
                    "file_change" => {
                        turn.tool_calls.push(ToolCall { id: id.clone(), name: "apply_patch".into(), input: json!({"changes": it["changes"]}) });
                        result = Some(ToolResult { tool_use_id: id, content: it["status"].as_str().unwrap_or("").into(), is_error: it["status"] == "failed" });
                    }
                    "mcp_tool_call" => {
                        let name = format!("mcp.{}.{}", it["server"].as_str().unwrap_or("?"), it["tool"].as_str().unwrap_or("?"));
                        turn.tool_calls.push(ToolCall { id, name, input: it["arguments"].clone() });
                    }
                    "web_search" => turn.tool_calls.push(ToolCall { id, name: "web_search".into(), input: json!({"query": it["query"]}) }),
                    "error" => {
                        rec.events.push(RunEvent { kind: "error".into(), at_ms: 0, detail: it["message"].as_str().unwrap_or("").into() });
                        continue;
                    }
                    _ => continue,
                }
                rec.transcript.turns.push(turn);
                if let Some(r) = result {
                    rec.transcript.turns.push(Turn { role: "user".into(), tool_results: vec![r], ..Default::default() });
                }
            }
            "turn.completed" => {
                turns += 1.0;
                if let Some(u) = ev["usage"].as_object() {
                    for (k, v) in u {
                        let key = match k.as_str() {
                            "input_tokens" => "input_tokens",
                            "cached_input_tokens" => "cache_read_tokens",
                            "cache_write_input_tokens" => "cache_creation_tokens",
                            "output_tokens" => "output_tokens",
                            "reasoning_output_tokens" => "thinking_tokens",
                            _ => continue,
                        };
                        *usage.entry(key).or_insert(0.0) += v.as_f64().unwrap_or(0.0);
                    }
                }
            }
            "turn.failed" => rec.error = Some(ev["error"]["message"].as_str().unwrap_or("turn failed").to_string()),
            "error" => rec.error = Some(ev["message"].as_str().unwrap_or("error").to_string()),
            _ => {}
        }
    }
    // Codex counts cached tokens inside input_tokens; split them so the
    // metric means the same thing across runners.
    let cached = usage.get("cache_read_tokens").copied().unwrap_or(0.0);
    if let Some(i) = usage.get_mut("input_tokens") {
        *i = (*i - cached).max(0.0);
    }
    for (k, v) in &usage {
        rec.metrics.insert(k.to_string(), *v);
    }
    let g = |k: &str| usage.get(k).copied().unwrap_or(0.0);
    let input_side = g("input_tokens") + g("cache_read_tokens") + g("cache_creation_tokens");
    if input_side > 0.0 {
        rec.metrics.insert("cache_hit".into(), g("cache_read_tokens") / input_side);
        rec.metrics.insert("total_tokens".into(), input_side + g("output_tokens"));
    }
    let assistant_msgs = rec.transcript.turns.iter().filter(|t| t.role == "assistant").count() as f64;
    rec.metrics.insert("turns".into(), assistant_msgs.max(turns));
    rec.final_text = rec.transcript.final_text();
    tally_tool_calls(&rec.transcript, &mut rec.metrics);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_jsonl() {
        let raw = r#"{"type":"thread.started","thread_id":"x"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I'll create it."}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cat .agents/skills/pirate/SKILL.md","aggregated_output":"arr","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"file_change","changes":[{"path":"/ws/hello.txt","kind":"add"}],"status":"completed"}}
{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Created hello.txt."}}
{"type":"turn.completed","usage":{"input_tokens":29918,"cached_input_tokens":27008,"cache_write_input_tokens":0,"output_tokens":224,"reasoning_output_tokens":0}}"#;
        let rec = parse_jsonl(raw);
        assert_eq!(rec.final_text, "Created hello.txt.");
        assert_eq!(rec.metrics["tool_calls"], 2.0);
        assert_eq!(rec.metrics["tool.shell"], 1.0);
        assert_eq!(rec.metrics["input_tokens"], 2910.0);
        assert_eq!(rec.metrics["cache_read_tokens"], 27008.0);
        assert_eq!(rec.transcript.skills_invoked(), vec!["pirate"]);
        assert!(rec.error.is_none());
    }
}

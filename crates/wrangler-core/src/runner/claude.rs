//! Claude Code runner.
//!
//! Isolation from the user's own setup rests on three flags, each checked by
//! the live isolation test (`tests/live_claude.rs`):
//! - `--setting-sources project` hides user-level settings, plugins, skills,
//!   hooks and `CLAUDE.md`.
//! - `--strict-mcp-config` hides MCP servers, including claude.ai connectors,
//!   which `--setting-sources` alone does not suppress.
//! - `--no-session-persistence` keeps runs out of the session history.
//!
//! Auth stays on the user's normal login. `--bare` and a fresh
//! `CLAUDE_CONFIG_DIR` both lose a Max subscription login and are not used.

use super::*;
use crate::isolation::Sandbox;
use crate::project::resolve_append;
use crate::transcript::{ToolCall, ToolResult, Turn};
use serde_json::Value;

#[derive(Default)]
pub struct ClaudeCodeRunner {
    /// Override the binary (tests).
    pub binary: Option<String>,
}

impl ClaudeCodeRunner {
    fn binary(&self) -> String {
        self.binary.clone().or_else(|| std::env::var("WRANGLER_CLAUDE_BIN").ok()).unwrap_or_else(|| "claude".into())
    }

    pub fn build_args(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Result<Vec<String>> {
        let mut a: Vec<String> = ["-p", "--setting-sources", "project", "--strict-mcp-config", "--no-session-persistence", "--permission-mode", "bypassPermissions"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(m) = &variant.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(e) = &variant.effort {
            a.extend(["--effort".into(), e.clone()]);
        }
        a.extend(["--output-format".into(), "stream-json".into(), "--verbose".into()]);
        let appends: Vec<String> = variant.appends().iter().map(|v| resolve_append(&ws.project, v)).collect::<Result<_>>()?;
        if !appends.is_empty() {
            a.extend(["--append-system-prompt".into(), appends.join("\n\n")]);
        }
        for p in &limits.cfg.plugin_dirs {
            a.extend(["--plugin-dir".into(), ws.project.abs(p).to_string_lossy().to_string()]);
        }
        if let Some(n) = limits.cfg.max_turns {
            a.extend(["--max-turns".into(), n.to_string()]);
        }
        for k in variant.knobs() {
            if let Some(flag) = &k.flag {
                a.push(flag.clone());
                // An empty value passes the flag alone; `off`/`false` omits it.
                match k.value.as_str() {
                    "" | "on" | "true" => {}
                    "off" | "false" => {
                        a.pop();
                    }
                    v => a.push(v.to_string()),
                }
            }
        }
        a.push("--".into());
        a.push(task.prompt.clone());
        Ok(a)
    }
}

impl AgentRunner for ClaudeCodeRunner {
    fn id(&self) -> &str {
        "claude-code"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            overlay: Support::Full,
            model: Support::Full,
            effort: Support::Full,
            system_prompt_append: Support::Full,
            knob: Support::Full,
            seatbelt: Support::Full,
            reports_cost: true,
        }
    }

    fn materialize(&self, ws: &Workspace, variant: &Variant) -> Result<()> {
        // Overlays are already laid out as project-level config
        // (./.claude/skills, ./CLAUDE.md, ./.claude/settings.json, ./.claude/agents).
        for o in variant.overlays() {
            ws.apply_overlay(o)?;
        }
        Ok(())
    }

    fn run(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Result<RunRecord> {
        let args = self.build_args(ws, task, variant, limits)?;
        let mut spec = ExecSpec {
            program: self.binary(),
            args,
            cwd: ws.dir.clone(),
            env: variant.knobs().iter().filter_map(|k| k.env.clone().map(|e| (e, k.value.clone()))).collect(),
            // When wrangler itself is driven by an agent, do not let the child
            // think it is nested inside that session.
            env_remove: vec!["CLAUDECODE".into(), "CLAUDE_CODE_ENTRYPOINT".into(), "CLAUDE_CODE_SSE_PORT".into()],
            stdout_path: ws.run_dir.join("raw.jsonl"),
            stderr_path: ws.run_dir.join("stderr.txt"),
            timeout: limits.timeout,
            cancel: limits.cancel.clone(),
        };
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.iter().cloned());
        let sandbox = Sandbox::for_claude(ws, limits)?;
        if let Some(sb) = &sandbox {
            sb.apply(&mut spec);
        }
        let out = exec(&spec)?;
        let mut rec = parse_stream(&out.stdout);
        if let Some(sb) = sandbox {
            rec.events.extend(sb.finish());
        }
        rec.command = command;
        rec.raw_output = out.stdout;
        rec.stderr = out.stderr;
        rec.exit_status = out.exit;
        rec.timed_out = out.timed_out;
        rec.cancelled = out.cancelled;
        rec.wall_ms = out.wall_ms;
        rec.metrics.insert("wall_time_s".into(), out.wall_ms as f64 / 1000.0);
        if !rec.timed_out && !rec.cancelled {
            let has_result = rec.metrics.contains_key("turns");
            if !has_result {
                let tail: String = rec.stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                if looks_rate_limited(&rec.stderr) {
                    rec.rate_limited = Some(tail.clone());
                }
                rec.error.get_or_insert(format!("claude exited with {:?} and no result event. {tail}", out.exit));
            }
        }
        Ok(rec)
    }
}

fn block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()).or_else(|| (i.get("type")?.as_str()? != "text").then(|| format!("[{}]", i["type"].as_str().unwrap_or("block")))))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Normalize Claude Code `stream-json` output: transcript, metrics from the
/// final result event, rate-limit events, and the error state.
pub fn parse_stream(raw: &str) -> RunRecord {
    let mut rec = RunRecord::default();
    let mut last_msg_id = String::new();
    for line in raw.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else { continue };
        let ty = ev["type"].as_str().unwrap_or("");
        let subagent = !ev["parent_tool_use_id"].is_null();
        match ty {
            "system" => {
                if ev["subtype"] == "init" {
                    rec.model = ev["model"].as_str().unwrap_or("").to_string();
                }
            }
            "assistant" => {
                let msg = &ev["message"];
                let id = msg["id"].as_str().unwrap_or("").to_string();
                // One event per content block; blocks of one message share an id.
                let merge = !id.is_empty() && id == last_msg_id && rec.transcript.turns.last().map(|t| t.role == "assistant").unwrap_or(false);
                if !merge {
                    rec.transcript.turns.push(Turn { role: "assistant".into(), subagent, ..Default::default() });
                }
                last_msg_id = id;
                let turn = rec.transcript.turns.last_mut().unwrap();
                for b in msg["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => {
                            if !turn.text.is_empty() {
                                turn.text.push('\n');
                            }
                            turn.text.push_str(b["text"].as_str().unwrap_or(""));
                        }
                        "thinking" => turn.thinking.push_str(b["thinking"].as_str().unwrap_or("")),
                        "tool_use" => turn.tool_calls.push(ToolCall {
                            id: b["id"].as_str().unwrap_or("").into(),
                            name: b["name"].as_str().unwrap_or("").into(),
                            input: b["input"].clone(),
                        }),
                        _ => {}
                    }
                }
            }
            "user" => {
                last_msg_id.clear();
                let mut turn = Turn { role: "user".into(), subagent, ..Default::default() };
                match &ev["message"]["content"] {
                    Value::String(s) => turn.text = s.clone(),
                    Value::Array(blocks) => {
                        for b in blocks {
                            match b["type"].as_str().unwrap_or("") {
                                "tool_result" => turn.tool_results.push(ToolResult {
                                    tool_use_id: b["tool_use_id"].as_str().unwrap_or("").into(),
                                    content: block_text(&b["content"]),
                                    is_error: b["is_error"].as_bool().unwrap_or(false),
                                }),
                                "text" => {
                                    if !turn.text.is_empty() {
                                        turn.text.push('\n');
                                    }
                                    turn.text.push_str(b["text"].as_str().unwrap_or(""));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                rec.transcript.turns.push(turn);
            }
            "rate_limit_event" => {
                let info = &ev["rate_limit_info"];
                let status = info["status"].as_str().unwrap_or("");
                if !status.starts_with("allowed") {
                    rec.events.push(RunEvent {
                        kind: "rate_limit".into(),
                        at_ms: 0,
                        detail: format!(
                            "status={status} type={} resets_at={}",
                            info["rateLimitType"].as_str().unwrap_or("?"),
                            info["resetsAt"]
                        ),
                    });
                }
            }
            "result" => {
                let m = &mut rec.metrics;
                let mut put = |k: &str, v: &Value| {
                    if let Some(f) = v.as_f64() {
                        m.insert(k.to_string(), f);
                    }
                };
                put("cost_usd", &ev["total_cost_usd"]);
                put("turns", &ev["num_turns"]);
                put("duration_ms", &ev["duration_ms"]);
                put("duration_api_ms", &ev["duration_api_ms"]);
                put("ttft_ms", &ev["ttft_ms"]);
                let u = &ev["usage"];
                put("input_tokens", &u["input_tokens"]);
                put("output_tokens", &u["output_tokens"]);
                put("cache_read_tokens", &u["cache_read_input_tokens"]);
                put("cache_creation_tokens", &u["cache_creation_input_tokens"]);
                put("thinking_tokens", &u["output_tokens_details"]["thinking_tokens"]);
                put("subagents_spawned", &ev["subagent_stats"]["spawned"]);
                if let Some(d) = ev["permission_denials"].as_array() {
                    m.insert("permission_denials".into(), d.len() as f64);
                }
                if let Some(models) = ev["modelUsage"].as_object() {
                    m.insert("models_used".into(), models.len() as f64);
                }
                let g = |k: &str| m.get(k).copied().unwrap_or(0.0);
                let (cache_read, output) = (g("cache_read_tokens"), g("output_tokens"));
                let input_side = g("input_tokens") + cache_read + g("cache_creation_tokens");
                if input_side > 0.0 {
                    m.insert("cache_hit".into(), cache_read / input_side);
                }
                m.insert("total_tokens".into(), input_side + output);
                rec.final_text = ev["result"].as_str().unwrap_or("").to_string();
                if ev["is_error"].as_bool().unwrap_or(false) || ev["subtype"].as_str().map(|s| s != "success").unwrap_or(false) {
                    let detail = format!(
                        "{}: {}",
                        ev["subtype"].as_str().unwrap_or("error"),
                        if rec.final_text.is_empty() { ev["terminal_reason"].as_str().unwrap_or("") } else { &rec.final_text }
                    );
                    let status = ev["api_error_status"].as_i64().unwrap_or(0);
                    if status == 429 || status == 529 || (ev["is_error"].as_bool().unwrap_or(false) && looks_rate_limited(&rec.final_text)) {
                        rec.rate_limited = Some(detail.clone());
                    }
                    rec.error = Some(detail);
                }
            }
            _ => {}
        }
    }
    if rec.final_text.is_empty() {
        rec.final_text = rec.transcript.final_text();
    }
    tally_tool_calls(&rec.transcript, &mut rec.metrics);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5","mcp_servers":[]}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"thinking","thinking":""}]}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Skill","input":{"skill":"pirate"}}]}}
{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Launching skill: pirate"},{"type":"text","text":"Base directory"}]}}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day"}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m2","content":[{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/ws/NOTES.md","content":"x"}}]}}
{"type":"user","parent_tool_use_id":null,"message":{"content":[{"tool_use_id":"t2","type":"tool_result","content":[{"type":"text","text":"File created"}]}]}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m3","content":[{"type":"text","text":"Done."}]}}
{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.004,"duration_ms":10211,"duration_api_ms":7994,"result":"Done.","usage":{"input_tokens":8,"cache_creation_input_tokens":100,"cache_read_input_tokens":892,"output_tokens":50,"output_tokens_details":{"thinking_tokens":20}},"permission_denials":[],"subagent_stats":{"spawned":0},"modelUsage":{"claude-haiku-5-5":{}}}"#;

    #[test]
    fn parses_stream_json() {
        let rec = parse_stream(SAMPLE);
        assert_eq!(rec.model, "claude-haiku-5-5");
        assert_eq!(rec.transcript.turns.len(), 5);
        assert_eq!(rec.transcript.turns[0].tool_calls[0].name, "Skill");
        assert_eq!(rec.transcript.turns[3].tool_results[0].content, "File created");
        assert_eq!(rec.final_text, "Done.");
        assert_eq!(rec.metrics["cost_usd"], 0.004);
        assert_eq!(rec.metrics["turns"], 3.0);
        assert_eq!(rec.metrics["tool_calls"], 2.0);
        assert_eq!(rec.metrics["tool.Write"], 1.0);
        assert_eq!(rec.metrics["thinking_tokens"], 20.0);
        assert!((rec.metrics["cache_hit"] - 0.892).abs() < 1e-9);
        assert_eq!(rec.metrics["total_tokens"], 1050.0);
        assert!(rec.events.is_empty() && rec.error.is_none() && rec.rate_limited.is_none());
        assert_eq!(rec.transcript.skills_invoked(), vec!["pirate"]);
    }

    #[test]
    fn detects_rate_limit_result() {
        let raw = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1791757200}}
{"type":"result","subtype":"error_during_execution","is_error":true,"api_error_status":429,"num_turns":0,"result":"Claude usage limit reached"}"#;
        let rec = parse_stream(raw);
        assert!(rec.rate_limited.is_some());
        assert_eq!(rec.events.len(), 1);
        assert_eq!(rec.events[0].kind, "rate_limit");
    }
}

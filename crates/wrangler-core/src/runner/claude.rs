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
use crate::transcript::{normalize_tool_name, ToolCall, ToolResult, Turn};
use serde_json::{json, Value};

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
        // Hook lifecycle events are only in the stream when asked for.
        a.extend(["--output-format".into(), "stream-json".into(), "--verbose".into(), "--include-hook-events".into()]);
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
            hooks: Support::Full,
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
        let docker = crate::isolation::DockerRun::wrap(ws, limits, &mut spec, &[])?;
        let out = exec(&spec)?;
        let mut rec = parse_stream(&out.stdout);
        if let Some(sb) = sandbox {
            rec.events.extend(sb.finish());
        }
        if let Some(d) = docker {
            rec.events.extend(d.finish());
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
                match ev["subtype"].as_str().unwrap_or("") {
                    "init" => rec.model = ev["model"].as_str().unwrap_or("").to_string(),
                    // One per hook execution, with `--include-hook-events`.
                    "hook_response" => {
                        let event = ev["hook_event"].as_str().unwrap_or("?").to_string();
                        // `hook_name` is "<Event>:<matcher>"; hooks without a matcher have only the event.
                        let matcher = ev["hook_name"].as_str().and_then(|n| n.split_once(':')).map(|(_, m)| m.to_string()).unwrap_or_default();
                        let output = crate::util::truncate_middle(ev["output"].as_str().or(ev["stdout"].as_str()).unwrap_or("").trim(), 2000);
                        let exit = ev["exit_code"].as_i64();
                        *rec.metrics.entry(format!("hook.{event}")).or_insert(0.0) += 1.0;
                        rec.events.push(RunEvent {
                            kind: "hook".into(),
                            at_ms: 0,
                            detail: format!("{event}{} exit {}", if matcher.is_empty() { String::new() } else { format!(" [{matcher}]") }, exit.map(|c| c.to_string()).unwrap_or_else(|| "?".into())),
                            data: json!({"event": event, "matcher": matcher, "exit_code": exit, "outcome": ev["outcome"], "output": output, "stderr": crate::util::truncate_middle(ev["stderr"].as_str().unwrap_or("").trim(), 1000)}),
                        });
                    }
                    _ => {}
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
                            name: normalize_tool_name(b["name"].as_str().unwrap_or("")),
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
                        ), ..Default::default() });
                }
            }
            "result" => {
                let m = &mut rec.metrics;
                // A conversation run has one result event per user turn. Cost
                // and API time are running totals; the rest are per turn.
                fn put(m: &mut BTreeMap<String, f64>, k: &str, v: &Value, cumulative: bool) {
                    if let Some(f) = v.as_f64() {
                        let e = m.entry(k.to_string()).or_insert(0.0);
                        *e = if cumulative { f.max(*e) } else { *e + f };
                    }
                }
                put(m, "cost_usd", &ev["total_cost_usd"], true);
                put(m, "turns", &ev["num_turns"], false);
                put(m, "duration_ms", &ev["duration_ms"], false);
                put(m, "duration_api_ms", &ev["duration_api_ms"], true);
                if !m.contains_key("ttft_ms") {
                    put(m, "ttft_ms", &ev["ttft_ms"], true);
                }
                let u = &ev["usage"];
                put(m, "input_tokens", &u["input_tokens"], false);
                put(m, "output_tokens", &u["output_tokens"], false);
                put(m, "cache_read_tokens", &u["cache_read_input_tokens"], false);
                put(m, "cache_creation_tokens", &u["cache_creation_input_tokens"], false);
                put(m, "thinking_tokens", &u["output_tokens_details"]["thinking_tokens"], false);
                put(m, "subagents_spawned", &ev["subagent_stats"]["spawned"], true);
                if let Some(d) = ev["permission_denials"].as_array() {
                    *m.entry("permission_denials".into()).or_insert(0.0) += d.len() as f64;
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
                if ev["subtype"] == "error_max_turns" {
                    rec.limit_hit = Some(format!("stopped at the max-turns limit after {} turns", ev["num_turns"]));
                    rec.final_text = rec.transcript.final_text();
                } else if ev["is_error"].as_bool().unwrap_or(false) || ev["subtype"].as_str().map(|s| s != "success").unwrap_or(false) {
                    let body = if rec.final_text.is_empty() { ev["terminal_reason"].as_str().unwrap_or("") } else { &rec.final_text };
                    // An error reported with subtype "success" (for example
                    // "Not logged in") reads better without the prefix.
                    let detail = match ev["subtype"].as_str() {
                        Some("success") | None => body.to_string(),
                        Some(sub) => format!("{sub}: {body}"),
                    };
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
pub(crate) const SAMPLE: &str = r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5","mcp_servers":[]}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"thinking","thinking":""}]}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Skill","input":{"skill":"pirate"}}]}}
{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Launching skill: pirate"},{"type":"text","text":"Base directory"}]}}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day"}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m2","content":[{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/ws/NOTES.md","content":"x"}}]}}
{"type":"user","parent_tool_use_id":null,"message":{"content":[{"tool_use_id":"t2","type":"tool_result","content":[{"type":"text","text":"File created"}]}]}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m3","content":[{"type":"text","text":"Done."}]}}
{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.004,"duration_ms":10211,"duration_api_ms":7994,"result":"Done.","usage":{"input_tokens":8,"cache_creation_input_tokens":100,"cache_read_input_tokens":892,"output_tokens":50,"output_tokens_details":{"thinking_tokens":20}},"permission_denials":[],"subagent_stats":{"spawned":0},"modelUsage":{"claude-haiku-5-5":{}}}"#;

#[cfg(test)]
mod tests {
    use super::*;


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
        assert_eq!((rec.metrics["tool_calls_before_final"], rec.metrics["distinct_tools"]), (2.0, 2.0));
    }

    #[test]
    fn hooks_mcp_names_and_turn_totals() {
        let raw = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"mcp__filesystem__read_file","input":{"path":"a"}}]}}
{"type":"system","subtype":"hook_started","hook_id":"h1","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse"}
{"type":"system","subtype":"hook_response","hook_id":"h1","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","output":"hook-ran\n","stdout":"hook-ran\n","stderr":"","exit_code":0,"outcome":"success"}
{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.001,"duration_ms":100,"result":"one","usage":{"input_tokens":2,"output_tokens":3}}
{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m2","content":[{"type":"text","text":"two"}]}}
{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.003,"duration_ms":50,"result":"two","usage":{"input_tokens":2,"output_tokens":4}}"#;
        let rec = parse_stream(raw);
        assert_eq!(rec.transcript.turns[0].tool_calls[0].name, "mcp.filesystem.read_file");
        assert_eq!(rec.metrics["mcp.filesystem.read_file"], 1.0);
        assert_eq!(rec.metrics["hook.PreToolUse"], 1.0);
        let hook = &rec.events[0];
        assert_eq!((hook.kind.as_str(), hook.data["matcher"].as_str(), hook.data["exit_code"].as_i64(), hook.data["output"].as_str()), ("hook", Some("Bash"), Some(0), Some("hook-ran")));
        // Cost is a running total across turns; turns and tokens add up.
        assert_eq!((rec.metrics["cost_usd"], rec.metrics["turns"], rec.metrics["output_tokens"], rec.metrics["duration_ms"]), (0.003, 3.0, 7.0, 150.0));
        assert_eq!(rec.final_text, "two");
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

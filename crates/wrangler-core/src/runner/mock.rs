//! A deterministic fake runner for tests, demos and UI work. It spends no
//! tokens. Output quality is a hidden number driven by `mock-effect: <x>`
//! lines in the materialized overlay, so experiments have a known answer.
//!
//! Directives read from `CLAUDE.md` and `.claude/skills/*/SKILL.md`:
//! - `mock-effect: 0.2` adds to quality (may be negative).
//! - `mock-trigger: never` makes the skill go uninvoked.

use super::*;
use crate::transcript::{ToolCall, ToolResult, Turn};
use crate::util::{hash_u64, Rng};
use serde_json::json;

pub struct MockRunner;

fn directive(text: &str, key: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string())).collect()
}

impl AgentRunner for MockRunner {
    fn id(&self) -> &str {
        "mock"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            overlay: Support::Full,
            model: Support::Full,
            effort: Support::Full,
            system_prompt_append: Support::Full,
            knob: Support::Full,
            seatbelt: Support::Approximate("the mock runner starts no process, so nothing is sandboxed"),
            hooks: Support::Unsupported("the mock runner runs no hooks"),
            reports_cost: true,
        }
    }

    fn materialize(&self, ws: &Workspace, variant: &Variant) -> Result<()> {
        for o in variant.overlays() {
            ws.apply_overlay(o)?;
        }
        Ok(())
    }

    fn run(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits) -> Result<RunRecord> {
        // Seeded by launch and run number (the last two parts of the run
        // directory), not the full path, so mock results are the same on
        // every machine and in every temp directory.
        let tail: Vec<String> = ws.run_dir.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        let seed = hash_u64(&format!("{}|{}|{}", task.id, variant.key, tail.join("/")));
        let mut rng = Rng::new(seed);
        let mut rec = RunRecord::default();
        let mut quality = 0.5 + ((hash_u64(&task.id) % 200) as f64 / 1000.0 - 0.1);
        let mut overlay_chars = 0usize;
        let mut turns: Vec<Turn> = vec![];

        let mut sources: Vec<(Option<String>, String)> = vec![];
        if let Ok(t) = std::fs::read_to_string(ws.dir.join("CLAUDE.md")) {
            sources.push((None, t));
        }
        if let Ok(rd) = std::fs::read_dir(ws.dir.join(".claude/skills")) {
            let mut dirs: Vec<_> = rd.flatten().collect();
            dirs.sort_by_key(|d| d.file_name());
            for d in dirs {
                if let Ok(t) = std::fs::read_to_string(d.path().join("SKILL.md")) {
                    sources.push((Some(d.file_name().to_string_lossy().to_string()), t));
                }
            }
        }
        for (skill, text) in &sources {
            overlay_chars += text.len();
            let never = directive(text, "mock-trigger:").iter().any(|v| v == "never");
            if let Some(name) = skill {
                if never {
                    continue;
                }
                let id = format!("call_{name}");
                turns.push(Turn {
                    role: "assistant".into(),
                    tool_calls: vec![ToolCall { id: id.clone(), name: "Skill".into(), input: json!({"skill": name}) }],
                    ..Default::default()
                });
                turns.push(Turn {
                    role: "user".into(),
                    tool_results: vec![ToolResult { tool_use_id: id, content: format!("Launching skill: {name}"), is_error: false }],
                    ..Default::default()
                });
            }
            for v in directive(text, "mock-effect:") {
                quality += v.parse::<f64>().unwrap_or(0.0);
            }
        }
        for a in variant.appends() {
            for v in directive(a, "mock-effect:") {
                quality += v.parse::<f64>().unwrap_or(0.0);
            }
        }
        let model = variant.model.clone().unwrap_or_else(|| "mock-model".into());
        if model.contains("opus") {
            quality += 0.1;
        }
        let effort_mult = match variant.effort.as_deref() {
            Some("high") | Some("xhigh") | Some("max") => {
                quality += 0.03;
                1.6
            }
            Some("low") => {
                quality -= 0.05;
                0.7
            }
            _ => 1.0,
        };
        quality = (quality + rng.normal() * 0.07).clamp(0.02, 0.98);

        let words = (220.0 - 150.0 * quality + rng.normal() * 6.0).max(20.0) as usize;
        let hedges = ((1.0 - quality) * 6.0) as usize;
        let mut body = format!("# Summary\n\nImpact first: checkout was down for some users. QUALITY={quality:.3}\n\n");
        let filler = ["status", "is", "stable", "and", "the", "team", "is", "monitoring", "recovery", "closely"];
        let mut line = String::new();
        for i in 0..words {
            if i < hedges * 9 && i % 9 == 0 {
                line.push_str("perhaps ");
            }
            line.push_str(filler[i % filler.len()]);
            line.push(' ');
            if i % 14 == 13 {
                body.push_str(line.trim_end());
                body.push_str(".\n");
                line.clear();
            }
        }
        body.push_str(line.trim_end());
        body.push('\n');
        let out_file = task.output_file.clone().unwrap_or_else(|| "OUTPUT.md".into());
        if let Some(p) = ws.resolve(&out_file) {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, &body)?;
        }
        turns.push(Turn {
            role: "assistant".into(),
            tool_calls: vec![ToolCall { id: "call_write".into(), name: "Write".into(), input: json!({"file_path": out_file, "content": body}) }],
            ..Default::default()
        });
        turns.push(Turn {
            role: "user".into(),
            tool_results: vec![ToolResult { tool_use_id: "call_write".into(), content: "File created".into(), is_error: false }],
            ..Default::default()
        });
        let final_text = format!("Wrote {out_file} ({words} words). QUALITY={quality:.3}");
        turns.push(Turn { role: "assistant".into(), text: final_text.clone(), ..Default::default() });

        let input = 9000.0 + overlay_chars as f64 / 4.0;
        let output = words as f64 * 1.4 * effort_mult;
        let rate = if model.contains("opus") { 5.0 } else { 1.0 };
        let m = &mut rec.metrics;
        m.insert("input_tokens".into(), 20.0);
        m.insert("cache_read_tokens".into(), input * 0.9);
        m.insert("cache_creation_tokens".into(), input * 0.1);
        m.insert("output_tokens".into(), output);
        m.insert("thinking_tokens".into(), 200.0 * effort_mult);
        m.insert("total_tokens".into(), input + 20.0 + output);
        m.insert("cache_hit".into(), 0.9);
        m.insert("cost_usd".into(), rate * (input * 3e-7 + output * 5e-6 * effort_mult));
        m.insert("turns".into(), turns.iter().filter(|t| t.role == "assistant").count() as f64);
        let wall = 4.0 * effort_mult + rng.f64() * 2.0;
        m.insert("wall_time_s".into(), wall);
        m.insert("mock_quality".into(), quality);

        let sleep_ms = std::env::var("WRANGLER_MOCK_SLEEP_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let start = std::time::Instant::now();
        while (start.elapsed().as_millis() as u64) < sleep_ms {
            if limits.cancel.load(Ordering::Relaxed) {
                rec.cancelled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        rec.transcript = Transcript { turns };
        tally_tool_calls(&rec.transcript, &mut rec.metrics);
        rec.raw_output = serde_json::to_string(&rec.transcript)?;
        rec.final_text = final_text;
        rec.exit_status = Some(0);
        rec.wall_ms = (wall * 1000.0) as u64;
        rec.model = model;
        rec.command = vec!["mock".into()];
        Ok(rec)
    }
}

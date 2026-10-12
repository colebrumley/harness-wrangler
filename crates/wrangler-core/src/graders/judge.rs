//! LLM judges: a rubric judge that scores one run against expectations, and
//! a blind pairwise judge that is run in both presentation orders so
//! position bias can be measured and cancelled.
//!
//! Judges run through Claude Code (`claude -p --json-schema`), with tools
//! disabled and the same isolation flags as agent runs. That keeps judging
//! on the user's subscription and gets schema-validated structured output.

use crate::model::{JudgeCfg, Task};
use crate::runner::{exec, looks_rate_limited, ExecSpec};
use crate::util::truncate_middle;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_OUTPUT_CHARS: usize = 60_000;
const MAX_DIFF_CHARS: usize = 30_000;
const MAX_TRACE_CHARS: usize = 12_000;

pub struct JudgeReply {
    pub value: Value,
    pub cost_usd: f64,
}

pub trait JudgeBackend: Send + Sync {
    fn complete(&self, prompt: &str, schema: &Value, cfg: &JudgeCfg, cancel: &Arc<AtomicBool>) -> Result<JudgeReply>;
}

pub fn backend_for(cfg: &JudgeCfg) -> Box<dyn JudgeBackend> {
    if cfg.model == "mock" {
        Box::new(MockJudge)
    } else {
        Box::new(ClaudeCodeJudge)
    }
}

pub struct ClaudeCodeJudge;

impl JudgeBackend for ClaudeCodeJudge {
    fn complete(&self, prompt: &str, schema: &Value, cfg: &JudgeCfg, cancel: &Arc<AtomicBool>) -> Result<JudgeReply> {
        let dir = crate::workspace::workspaces_root().join(format!("judge-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        let mut args: Vec<String> = ["-p", "--setting-sources", "project", "--strict-mcp-config", "--no-session-persistence", "--tools", "", "--model"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.push(cfg.model.clone());
        if let Some(e) = &cfg.effort {
            args.extend(["--effort".into(), e.clone()]);
        }
        args.extend(["--output-format".into(), "json".into(), "--json-schema".into(), schema.to_string(), "--".into(), prompt.to_string()]);
        let bin = std::env::var("WRANGLER_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
        let mut last_err = anyhow!("judge did not run");
        for attempt in 0..4u32 {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let out = exec(&ExecSpec {
                program: bin.clone(),
                args: args.clone(),
                cwd: dir.clone(),
                env: vec![],
                env_remove: vec!["CLAUDECODE".into(), "CLAUDE_CODE_ENTRYPOINT".into(), "CLAUDE_CODE_SSE_PORT".into()],
                stdout_path: dir.join("out.json"),
                stderr_path: dir.join("err.txt"),
                timeout: Duration::from_secs(600),
                cancel: cancel.clone(),
            })?;
            let parsed: Option<Value> = serde_json::from_str(&out.stdout).ok();
            if let Some(v) = &parsed {
                let structured = match &v["structured_output"] {
                    Value::Null => v["result"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()),
                    s => Some(s.clone()),
                };
                if let (false, Some(value)) = (v["is_error"].as_bool().unwrap_or(false), structured) {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Ok(JudgeReply { value, cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0) });
                }
            }
            let detail = parsed
                .as_ref()
                .and_then(|v| v["result"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("exit {:?}: {}", out.exit, out.stderr.lines().last().unwrap_or("")));
            let limited = looks_rate_limited(&detail) || parsed.as_ref().map(|v| v["api_error_status"] == 429 || v["api_error_status"] == 529).unwrap_or(false);
            last_err = anyhow!("judge call failed: {}", truncate_middle(&detail, 400));
            if out.cancelled {
                break;
            }
            let wait = if limited { 30u64 << attempt } else { 2 };
            for _ in 0..wait * 10 {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        Err(last_err)
    }
}

/// Offline judge paired with the mock runner: reads the hidden `QUALITY=`
/// marker the mock runner writes. It has a deliberate small first-position
/// bias so the position-bias report has something to show.
pub struct MockJudge;

fn quality_in(text: &str) -> Option<f64> {
    let i = text.find("QUALITY=")?;
    let rest = &text[i + 8..];
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    num.parse().ok()
}

fn section<'a>(prompt: &'a str, tag: &str) -> &'a str {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    match (prompt.find(&open), prompt.rfind(&close)) {
        (Some(a), Some(b)) if b > a => &prompt[a + open.len()..b],
        _ => "",
    }
}

impl JudgeBackend for MockJudge {
    fn complete(&self, prompt: &str, schema: &Value, _cfg: &JudgeCfg, _cancel: &Arc<AtomicBool>) -> Result<JudgeReply> {
        let value = if schema["properties"].get("should").is_some() {
            // Trigger corpus drafts: should-fire prompts reuse the description's own words, near misses avoid them.
            let words: Vec<String> = section(prompt, "description").split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 6).map(|w| w.to_lowercase()).collect();
            let n = prompt.split("lists of ").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|x| x.parse::<usize>().ok()).unwrap_or(4);
            let word = |i: usize| words.get(i % words.len().max(1)).cloned().unwrap_or_else(|| "thing".into());
            json!({
                "should": (0..n).map(|i| format!("Please help with {} for my team, draft {}.", word(i), i + 1)).collect::<Vec<_>>(),
                "should_not": (0..n).map(|i| format!("What is the history of topic {} in general?", i + 1)).collect::<Vec<_>>(),
            })
        } else if schema["properties"].get("expected_output").is_some() {
            json!({"expected_output": "The task is done as the session did it (mock draft).", "expectations": ["The result does what the prompt asks", "The tests the session ran pass"]})
        } else if schema["properties"].get("winner").is_some() {
            let a = quality_in(section(prompt, "output_a")).unwrap_or(0.5);
            let b = quality_in(section(prompt, "output_b")).unwrap_or(0.5);
            let winner = if (a - b).abs() < 0.02 {
                "tie"
            } else if a + 0.05 >= b {
                "A"
            } else {
                "B"
            };
            json!({"winner": winner, "reason": format!("mock judge: A={a:.2} B={b:.2}")})
        } else {
            let q = quality_in(section(prompt, "output")).unwrap_or(0.5);
            let texts: Vec<String> = section(prompt, "expectations").lines().filter_map(|l| l.trim().split_once(". ").map(|(_, t)| t.to_string())).collect();
            let exps: Vec<Value> = texts
                .iter()
                .enumerate()
                .map(|(i, t)| json!({"text": t, "passed": q > 0.3 + 0.25 * i as f64, "evidence": format!("mock quality {q:.2}")}))
                .collect();
            json!({"expectations": exps, "score": (1.0 + 9.0 * q).round(), "summary": format!("mock judge: quality {q:.2}")})
        };
        Ok(JudgeReply { value, cost_usd: 0.0 })
    }
}

// ---------------------------------------------------------------- rubric

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectationResult {
    pub text: String,
    pub passed: bool,
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricResult {
    #[serde(default)]
    pub expectations: Vec<ExpectationResult>,
    /// Overall quality, 1 (unusable) to 10 (excellent).
    pub score: f64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub cost_usd: f64,
}

impl RubricResult {
    pub fn pass_fraction(&self) -> Option<f64> {
        if self.expectations.is_empty() {
            return None;
        }
        Some(self.expectations.iter().filter(|e| e.passed).count() as f64 / self.expectations.len() as f64)
    }
}

/// What a judge (or a human reviewer) is shown for one run.
#[derive(Debug, Clone, Default)]
pub struct Shown {
    pub output: String,
    pub diff: String,
    /// True when the output is a file in the workspace rather than the final message.
    pub output_is_file: bool,
    /// Compact tool-call trace, shown only with `judge = { transcript = "tools" }`.
    pub trace: String,
}

fn task_block(task: &Task, rubric: Option<&str>) -> String {
    let mut s = format!("<task_prompt>\n{}\n</task_prompt>\n", task.prompt.trim());
    if !task.expected_output.trim().is_empty() {
        s.push_str(&format!("\n<expected_output>\n{}\n</expected_output>\n", task.expected_output.trim()));
    }
    if let Some(r) = rubric {
        s.push_str(&format!("\n<rubric>\n{}\n</rubric>\n", r.trim()));
    }
    s
}

/// A result that was accepted for this task before. Shown to the rubric
/// judge only: in a blind A/B it would anchor both sides the same way.
fn reference_block(task: &Task) -> String {
    match task.reference_output.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("\n<reference_output>\n{}\n</reference_output>\nThe reference is one result that was accepted for this task. Use it to calibrate what good looks like; a result need not resemble it to score well.\n", truncate_middle(r, MAX_DIFF_CHARS)),
        None => String::new(),
    }
}

fn shown_block(tag: &str, shown: &Shown) -> String {
    let mut s = format!("<{tag}>\n{}\n</{tag}>\n", truncate_middle(shown.output.trim(), MAX_OUTPUT_CHARS));
    // When the output is itself a workspace file its content is already
    // above; the diff adds only the other files the agent changed.
    if !shown.diff.trim().is_empty() {
        s.push_str(&format!("<{tag}_workspace_diff>\n{}\n</{tag}_workspace_diff>\n", truncate_middle(shown.diff.trim(), MAX_DIFF_CHARS)));
    }
    // The process, when the experiment asks for it: tool names and one-line inputs, in order.
    if !shown.trace.trim().is_empty() {
        s.push_str(&format!("<{tag}_tool_calls>\n{}\n</{tag}_tool_calls>\n", truncate_middle(shown.trace.trim(), MAX_TRACE_CHARS)));
    }
    s
}

pub fn rubric_prompt(task: &Task, rubric: Option<&str>, shown: &Shown) -> String {
    let mut s = String::from(
        "You are grading the result of a task given to an AI coding agent. Judge only what is in front of you: the task, and what the agent produced. Be strict and literal. Do not give credit for intent.\n\n",
    );
    s.push_str(&task_block(task, rubric));
    if !task.expectations.is_empty() {
        s.push_str("\n<expectations>\n");
        for (i, e) in task.expectations.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", i + 1, e));
        }
        s.push_str("</expectations>\n");
    }
    s.push_str(&reference_block(task));
    s.push_str("\nThe agent's result:\n\n");
    s.push_str(&shown_block("output", shown));
    s.push_str(
        "\nReturn:\n- `expectations`: one entry per numbered expectation, in order, with its text, whether the result meets it (`passed`), and `evidence`: a short quote from the result or a specific observation that justifies the verdict. Use an empty list if there are no numbered expectations.\n- `score`: overall quality from 1 (unusable) to 10 (excellent), weighing the expected output and the rubric if given.\n- `summary`: one sentence on the main strength or weakness.\n",
    );
    s
}

pub fn rubric_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "expectations": {"type": "array", "items": {"type": "object", "properties": {
                "text": {"type": "string"}, "passed": {"type": "boolean"}, "evidence": {"type": "string"}},
                "required": ["text", "passed", "evidence"]}},
            "score": {"type": "number", "minimum": 1, "maximum": 10},
            "summary": {"type": "string"}
        },
        "required": ["expectations", "score", "summary"]
    })
}

pub fn score_run(backend: &dyn JudgeBackend, cfg: &JudgeCfg, task: &Task, rubric: Option<&str>, shown: &Shown, cancel: &Arc<AtomicBool>) -> Result<RubricResult> {
    let reply = backend.complete(&rubric_prompt(task, rubric, shown), &rubric_schema(), cfg, cancel)?;
    let mut r: RubricResult = serde_json::from_value(reply.value)?;
    r.score = r.score.clamp(1.0, 10.0);
    // Keep the task's own wording so results line up across runs.
    for (e, text) in r.expectations.iter_mut().zip(&task.expectations) {
        e.text = text.clone();
    }
    r.expectations.truncate(task.expectations.len());
    r.cost_usd = reply.cost_usd;
    Ok(r)
}

// -------------------------------------------------------------- pairwise

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResult {
    /// a | b | tie, in terms of the caller's run A and run B.
    pub winner: String,
    /// Did both presentation orders pick the same run?
    pub consistent: bool,
    /// Which position (A or B) was picked in each order, and why.
    pub first_pick: String,
    pub first_reason: String,
    pub second_pick: String,
    pub second_reason: String,
    pub cost_usd: f64,
}

pub fn pairwise_prompt(task: &Task, rubric: Option<&str>, first: &Shown, second: &Shown) -> String {
    let mut s = String::from(
        "Two AI coding agents were given the same task. Compare their results and pick the better one. You do not know how either was configured. Judge only the results against the task. Ignore which is listed first.\n\n",
    );
    s.push_str(&task_block(task, rubric));
    if !task.expectations.is_empty() {
        s.push_str("\n<expectations>\n");
        for (i, e) in task.expectations.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", i + 1, e));
        }
        s.push_str("</expectations>\n");
    }
    s.push_str("\nResult A:\n\n");
    s.push_str(&shown_block("output_a", first));
    s.push_str("\nResult B:\n\n");
    s.push_str(&shown_block("output_b", second));
    s.push_str("\nReturn `winner`: \"A\", \"B\", or \"tie\" if neither is meaningfully better, and `reason`: one sentence naming the deciding difference.\n");
    s
}

pub fn pairwise_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"winner": {"type": "string", "enum": ["A", "B", "tie"]}, "reason": {"type": "string"}},
        "required": ["winner", "reason"]
    })
}

/// Blind A/B in both orders. The combined verdict is a win only when both
/// orders pick the same run; a split is recorded as a tie with
/// `consistent = false`.
pub fn compare(backend: &dyn JudgeBackend, cfg: &JudgeCfg, task: &Task, rubric: Option<&str>, a: &Shown, b: &Shown, cancel: &Arc<AtomicBool>) -> Result<PairResult> {
    let ask = |first: &Shown, second: &Shown| -> Result<(String, String, f64)> {
        let reply = backend.complete(&pairwise_prompt(task, rubric, first, second), &pairwise_schema(), cfg, cancel)?;
        let pick = reply.value["winner"].as_str().unwrap_or("tie").to_string();
        Ok((pick, reply.value["reason"].as_str().unwrap_or("").to_string(), reply.cost_usd))
    };
    let (p1, r1, c1) = ask(a, b)?;
    let (p2, r2, c2) = ask(b, a)?;
    // Map each positional pick back to the caller's run a / run b.
    let v1 = match p1.as_str() {
        "A" => "a",
        "B" => "b",
        _ => "tie",
    };
    let v2 = match p2.as_str() {
        "A" => "b",
        "B" => "a",
        _ => "tie",
    };
    let (winner, consistent) = if v1 == v2 { (v1, true) } else { ("tie", false) };
    Ok(PairResult { winner: winner.into(), consistent, first_pick: p1, first_reason: r1, second_pick: p2, second_reason: r2, cost_usd: c1 + c2 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task::parse("id='t'\nprompt='Summarize'\nexpectations=['Leads with impact','Under 150 words','No hedging']\n").unwrap()
    }
    fn cfg() -> JudgeCfg {
        JudgeCfg { model: "mock".into(), effort: None, rubric: None, rubric_scoring: true, pairwise: true, pairwise_mode: None, pairwise_reps: None, transcript: None }
    }
    fn shown(q: f64) -> Shown {
        Shown { output: format!("text QUALITY={q:.3}"), ..Default::default() }
    }

    #[test]
    fn mock_rubric_and_pairwise() {
        let cancel = Arc::new(AtomicBool::new(false));
        let r = score_run(&MockJudge, &cfg(), &task(), Some("be plain"), &shown(0.7), &cancel).unwrap();
        assert_eq!(r.expectations.len(), 3);
        assert_eq!(r.expectations[0].text, "Leads with impact");
        assert_eq!(r.score, 7.0);
        assert!((r.pass_fraction().unwrap() - 2.0 / 3.0).abs() < 1e-9);

        let clear = compare(&MockJudge, &cfg(), &task(), None, &shown(0.4), &shown(0.8), &cancel).unwrap();
        assert_eq!((clear.winner.as_str(), clear.consistent), ("b", true));
        // Within the mock's first-position bias: each order picks position A.
        let close = compare(&MockJudge, &cfg(), &task(), None, &shown(0.50), &shown(0.53), &cancel).unwrap();
        assert_eq!((close.winner.as_str(), close.consistent), ("tie", false));
        assert_eq!((close.first_pick.as_str(), close.second_pick.as_str()), ("A", "A"));
    }

    #[test]
    fn prompts_carry_the_material() {
        let p = pairwise_prompt(&task(), Some("RUBRIC-TEXT"), &shown(0.1), &Shown { output: "o".into(), diff: "+added".into(), output_is_file: true, ..Default::default() });
        assert!(p.contains("RUBRIC-TEXT") && p.contains("<output_a>") && p.contains("<output_b_workspace_diff>") && p.contains("2. Under 150 words"));
        assert!(!p.to_lowercase().contains("skill"), "pairwise prompt must stay blind to configuration");
        assert!(!p.contains("_tool_calls>"), "no trace unless asked for");
        // A harvested reference is shown to the rubric judge, and only to it.
        let mut with_ref = task();
        with_ref.reference_output = Some("REFERENCE-RESULT".into());
        assert!(rubric_prompt(&with_ref, None, &shown(0.5)).contains("<reference_output>\nREFERENCE-RESULT\n</reference_output>"));
        assert!(!pairwise_prompt(&with_ref, None, &shown(0.5), &shown(0.4)).contains("REFERENCE-RESULT"));
        assert!(!rubric_prompt(&task(), None, &shown(0.5)).contains("reference_output"));
        let with = Shown { output: "o".into(), trace: "1. Bash: pytest -q\n".into(), ..Default::default() };
        let p = rubric_prompt(&task(), None, &with);
        assert!(p.contains("<output_tool_calls>\n1. Bash: pytest -q\n</output_tool_calls>"));
        assert!(pairwise_prompt(&task(), None, &with, &shown(0.1)).contains("<output_a_tool_calls>"));
    }
}

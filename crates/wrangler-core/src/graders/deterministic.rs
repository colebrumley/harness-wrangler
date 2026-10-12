//! Deterministic graders: command exit code, file exists, regex, diff size,
//! word count, JSON schema, skill invoked, and the transcript graders (tool
//! calls, call sequences, transcript regex, hooks fired).

use crate::model::{DetSpec, Names};
use crate::runner::{exec, ExecSpec, RunEvent};
use crate::store::Grade;
use crate::transcript::{input_summary, CallAt, Transcript};
use crate::util::word_count;
use crate::workspace::{DiffStats, Workspace};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub struct GradeInput<'a> {
    pub ws: &'a Workspace,
    /// The task's output file content, or the final message.
    pub output: &'a str,
    pub diff: &'a DiffStats,
    pub transcript: &'a Transcript,
    /// Hook executions the runner saw, or `None` when the runner does not
    /// report hooks at all.
    pub hooks: Option<&'a [RunEvent]>,
    /// Index of this grader, used for a unique default name and scratch files.
    pub index: usize,
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn in_bounds(v: u64, min: Option<u64>, max: Option<u64>) -> bool {
    min.map(|m| v >= m).unwrap_or(true) && max.map(|m| v <= m).unwrap_or(true)
}

/// Text a grader reads: a workspace file when `path` is given, else the output.
fn target_text(input: &GradeInput, path: &Option<String>) -> Result<String, String> {
    match path {
        Some(p) => input.ws.read_file(p).ok_or_else(|| format!("file `{p}` not found")),
        None => Ok(input.output.to_string()),
    }
}

fn call_json(c: &CallAt) -> Value {
    json!({"id": c.call.id, "turn": c.turn, "name": c.call.name, "input": input_summary(&c.call.input, 160)})
}

/// Does a call match a tool name and an optional regex over its serialized input?
fn call_matches(c: &CallAt, names: &Names, input: Option<&regex::Regex>) -> bool {
    names.matches(&c.call.name) && input.map(|re| re.is_match(&c.call.input.to_string())).unwrap_or(true)
}

fn compile(pattern: &Option<String>) -> Result<Option<regex::Regex>, String> {
    match pattern {
        Some(p) => regex::Regex::new(p).map(Some).map_err(|e| format!("bad input pattern `{p}`: {e}")),
        None => Ok(None),
    }
}

/// Run one grader. Never errors: a grader that cannot run is a failed grade
/// with the reason in its detail.
pub fn grade(spec: &DetSpec, input: &GradeInput, run_id: i64) -> Grade {
    let default_name = match spec {
        DetSpec::FileExists { path, .. } => format!("file_exists:{path}"),
        DetSpec::WordCount { path: Some(p), .. } | DetSpec::Regex { path: Some(p), .. } | DetSpec::JsonSchema { path: Some(p), .. } => {
            format!("{}:{p}", spec.kind())
        }
        DetSpec::SkillInvoked { skill: Some(s), .. } => format!("skill_invoked:{s}"),
        DetSpec::ToolCall { name, .. } => format!("tool_call:{}", name.display()),
        DetSpec::HookFired { event, .. } => format!("hook_fired:{event}"),
        _ => spec.kind().to_string(),
    };
    let grader = spec.name().map(|s| s.to_string()).unwrap_or(default_name);
    let (passed, score, detail): (bool, Option<f64>, Value) = match spec {
        DetSpec::Command { cmd, expect_exit, timeout_secs, .. } => {
            let out = exec(&ExecSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), cmd.clone()],
                cwd: input.ws.dir.clone(),
                env: vec![],
                env_remove: vec![],
                stdout_path: input.ws.run_dir.join(format!("grader-{}.out", input.index)),
                stderr_path: input.ws.run_dir.join(format!("grader-{}.err", input.index)),
                timeout: Duration::from_secs(timeout_secs.unwrap_or(300)),
                cancel: Arc::new(AtomicBool::new(false)),
            });
            match out {
                Ok(o) => (
                    !o.timed_out && o.exit == Some(*expect_exit),
                    o.exit.map(|c| c as f64),
                    json!({"cmd": cmd, "exit": o.exit, "timed_out": o.timed_out, "stdout": tail(&o.stdout, 30), "stderr": tail(&o.stderr, 30)}),
                ),
                Err(e) => (false, None, json!({"cmd": cmd, "error": e.to_string()})),
            }
        }
        DetSpec::FileExists { path, .. } => {
            let exists = input.ws.resolve(path).map(|p| p.exists()).unwrap_or(false);
            (exists, None, json!({"path": path}))
        }
        DetSpec::Regex { pattern, path, should_match, .. } => match (regex::Regex::new(pattern), target_text(input, path)) {
            (Ok(re), Ok(text)) => {
                let m = re.find(&text).map(|m| m.as_str().to_string());
                (m.is_some() == *should_match, None, json!({"pattern": pattern, "should_match": should_match, "matched": m}))
            }
            (Err(e), _) => (false, None, json!({"error": format!("bad pattern: {e}")})),
            (_, Err(e)) => (false, None, json!({"error": e})),
        },
        DetSpec::DiffSize { min, max, .. } => {
            let n = input.diff.added + input.diff.removed;
            (in_bounds(n, *min, *max), Some(n as f64), json!({"changed_lines": n, "min": min, "max": max}))
        }
        DetSpec::WordCount { path, min, max, .. } => match target_text(input, path) {
            Ok(text) => {
                let n = word_count(&text) as u64;
                (in_bounds(n, *min, *max), Some(n as f64), json!({"words": n, "min": min, "max": max}))
            }
            Err(e) => (false, None, json!({"error": e})),
        },
        DetSpec::JsonSchema { path, schema_file, schema, .. } => {
            let schema_val: Result<Value, String> = match (schema, schema_file) {
                (Some(s), _) => Ok(s.clone()),
                (None, Some(f)) => std::fs::read_to_string(input.ws.project.abs(f))
                    .map_err(|e| format!("schema file {f}: {e}"))
                    .and_then(|t| serde_json::from_str(&t).map_err(|e| format!("schema file {f}: {e}"))),
                (None, None) => Err("json_schema grader needs `schema` or `schema_file`".into()),
            };
            let instance = target_text(input, path).and_then(|t| {
                // Tolerate a fenced block around JSON in a final message.
                let t = t.trim();
                let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).map(|x| x.trim_end_matches("```")).unwrap_or(t);
                serde_json::from_str::<Value>(t.trim()).map_err(|e| format!("not valid JSON: {e}"))
            });
            match (schema_val, instance) {
                (Ok(s), Ok(i)) => match jsonschema::validator_for(&s) {
                    Ok(v) => {
                        let errors: Vec<String> = v.iter_errors(&i).take(10).map(|e| format!("{}: {e}", e.instance_path)).collect();
                        (errors.is_empty(), None, json!({"errors": errors}))
                    }
                    Err(e) => (false, None, json!({"error": format!("invalid schema: {e}")})),
                },
                (Err(e), _) | (_, Err(e)) => (false, None, json!({"error": e})),
            }
        }
        DetSpec::SkillInvoked { skill, expect, .. } => {
            let invoked = input.transcript.skills_invoked();
            let hit = match skill {
                Some(s) => invoked.iter().any(|i| i == s),
                None => !invoked.is_empty(),
            };
            (hit == *expect, Some(if hit { 1.0 } else { 0.0 }), json!({"invoked": invoked, "expect": expect}))
        }
        DetSpec::ToolCall { name, input: pattern, min, max, subagents, .. } => match compile(pattern) {
            Err(e) => (false, None, json!({"error": e})),
            Ok(re) => {
                let calls = input.transcript.tool_calls_with_turn();
                let matched: Vec<&CallAt> = calls
                    .iter()
                    .filter(|c| match subagents.as_str() {
                        "exclude" => !c.subagent,
                        "only" => c.subagent,
                        _ => true,
                    })
                    .filter(|c| call_matches(c, name, re.as_ref()))
                    .collect();
                let n = matched.len() as u64;
                (
                    in_bounds(n, *min, *max),
                    Some(n as f64),
                    json!({"count": n, "min": min, "max": max, "tool": name.list(), "input": pattern, "subagents": subagents,
                        "call_ids": matched.iter().map(|c| c.call.id.clone()).collect::<Vec<_>>(),
                        "calls": matched.iter().take(20).map(|c| call_json(c)).collect::<Vec<_>>()}),
                )
            }
        },
        DetSpec::Sequence { steps, before_final, .. } => {
            let compiled: Result<Vec<Option<regex::Regex>>, String> = steps.iter().map(|s| compile(&s.input)).collect();
            match compiled {
                Err(e) => (false, None, json!({"error": e})),
                Ok(res) => {
                    let calls = input.transcript.tool_calls_with_turn();
                    let final_turn = input.transcript.final_turn();
                    let end = match (before_final, final_turn) {
                        (true, Some(t)) => t,
                        _ => input.transcript.turns.len(),
                    };
                    // Greedy in-order match: each step takes the first matching call after the previous step's.
                    let mut from = 0usize;
                    let mut matched: Vec<&CallAt> = vec![];
                    let mut missing: Option<usize> = None;
                    for (i, (step, re)) in steps.iter().zip(&res).enumerate() {
                        match calls.iter().enumerate().skip(from).find(|(_, c)| c.turn < end && call_matches(c, &step.name, re.as_ref())) {
                            Some((pos, c)) => {
                                matched.push(c);
                                from = pos + 1;
                            }
                            None => {
                                missing = Some(i);
                                break;
                            }
                        }
                    }
                    // Say why the missing step did not match: never called, out of order, or only after the final message.
                    let why = missing.map(|i| {
                        let anywhere: Vec<&CallAt> = calls.iter().filter(|c| call_matches(c, &steps[i].name, res[i].as_ref())).collect();
                        if anywhere.is_empty() {
                            "no matching tool call"
                        } else if anywhere.iter().all(|c| c.turn >= end) {
                            "matched only after the final message"
                        } else {
                            "matched only before an earlier step (out of order)"
                        }
                    });
                    (
                        missing.is_none() && !steps.is_empty(),
                        Some(matched.len() as f64),
                        json!({"steps": steps.len(), "matched_steps": matched.len(), "before_final": before_final, "final_turn": final_turn,
                            "missing_step": missing.map(|i| json!({"index": i + 1, "tool": steps[i].name.list(), "input": steps[i].input, "why": why})),
                            "call_ids": matched.iter().map(|c| c.call.id.clone()).collect::<Vec<_>>(),
                            "calls": matched.iter().map(|c| call_json(c)).collect::<Vec<_>>()}),
                    )
                }
            }
        }
        DetSpec::TranscriptRegex { pattern, scope, should_match, .. } => match regex::Regex::new(pattern) {
            Err(e) => (false, None, json!({"error": format!("bad pattern: {e}")})),
            Ok(re) => {
                let (assistant, results, thinking) = match scope.as_str() {
                    "assistant" => (true, false, false),
                    "tool_results" => (false, true, false),
                    "thinking" => (false, false, true),
                    _ => (true, true, true),
                };
                let mut hit: Option<(usize, String)> = None;
                'turns: for (i, t) in input.transcript.turns.iter().enumerate() {
                    let mut texts: Vec<&str> = vec![];
                    // `all` also covers user turns, so a dialog can be searched whole.
                    if (assistant && t.role == "assistant") || (scope == "all" && t.role != "assistant") {
                        texts.push(&t.text);
                    }
                    if thinking {
                        texts.push(&t.thinking);
                    }
                    if results {
                        texts.extend(t.tool_results.iter().map(|r| r.content.as_str()));
                    }
                    for text in texts {
                        if let Some(m) = re.find(text) {
                            hit = Some((i, crate::util::truncate_middle(m.as_str(), 200)));
                            break 'turns;
                        }
                    }
                }
                (hit.is_some() == *should_match, None, json!({"pattern": pattern, "scope": scope, "should_match": should_match, "matched": hit.as_ref().map(|h| h.1.clone()), "turn": hit.as_ref().map(|h| h.0)}))
            }
        },
        DetSpec::HookFired { event, min, max, .. } => match input.hooks {
            None => (false, None, json!({"error": "runner does not report hooks", "event": event})),
            Some(events) => {
                let fired: Vec<&RunEvent> = events.iter().filter(|e| e.kind == "hook" && e.data["event"].as_str() == Some(event.as_str())).collect();
                let n = fired.len() as u64;
                (in_bounds(n, *min, *max), Some(n as f64), json!({"event": event, "count": n, "min": min, "max": max, "hooks": fired.iter().take(20).map(|e| e.data.clone()).collect::<Vec<_>>()}))
            }
        },
    };
    Grade { run_id, grader, tier: "deterministic".into(), passed: Some(passed), score, detail }
}

/// Grade a run with every spec, keeping grader names unique.
pub fn grade_all(specs: &[DetSpec], ws: &Workspace, output: &str, diff: &DiffStats, transcript: &Transcript, hooks: Option<&[RunEvent]>, run_id: i64) -> Vec<Grade> {
    let mut out: Vec<Grade> = vec![];
    for (index, spec) in specs.iter().enumerate() {
        let mut g = grade(spec, &GradeInput { ws, output, diff, transcript, hooks, index }, run_id);
        if out.iter().any(|o| o.grader == g.grader) {
            g.grader = format!("{}#{}", g.grader, index + 1);
        }
        out.push(g);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;
    use crate::project::Project;
    use crate::transcript::{ToolCall, Turn};

    fn specs(toml_list: &str) -> Vec<DetSpec> {
        #[derive(serde::Deserialize)]
        struct W {
            g: Vec<DetSpec>,
        }
        toml::from_str::<W>(&format!("g = {toml_list}")).unwrap().g
    }

    #[test]
    fn all_grader_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::init(tmp.path()).unwrap();
        let task = Task::parse("id='t'\nprompt='p'\n").unwrap();
        let ws = Workspace::create(&project, &task, &tmp.path().join("run"), "g").unwrap();
        ws.commit_baseline().unwrap();
        std::fs::write(ws.dir.join("SUMMARY.md"), "Checkout was down for ten minutes. It is fixed.").unwrap();
        std::fs::write(ws.dir.join("data.json"), r#"{"score": 7, "tags": ["a"]}"#).unwrap();
        let (_, diff) = ws.snapshot().unwrap();
        let transcript = Transcript {
            turns: vec![Turn {
                role: "assistant".into(),
                tool_calls: vec![ToolCall { id: "1".into(), name: "Skill".into(), input: json!({"skill": "rfw"}) }],
                ..Default::default()
            }],
        };
        let list = specs(
            r#"[
            { kind = "command", cmd = "test -f SUMMARY.md" },
            { kind = "command", cmd = "exit 3", name = "fails" },
            { kind = "file_exists", path = "SUMMARY.md" },
            { kind = "file_exists", path = "missing.md" },
            { kind = "regex", pattern = "(?i)perhaps|maybe", path = "SUMMARY.md", should_match = false },
            { kind = "regex", pattern = "final \\w+" },
            { kind = "diff_size", max = 5 },
            { kind = "word_count", path = "SUMMARY.md", max = 150 },
            { kind = "word_count", path = "SUMMARY.md", max = 3, name = "too-long" },
            { kind = "json_schema", path = "data.json", schema = { type = "object", required = ["score"], properties = { score = { type = "integer" } } } },
            { kind = "json_schema", path = "data.json", name = "bad-shape", schema = { type = "object", required = ["nope"] } },
            { kind = "skill_invoked", skill = "rfw" },
            { kind = "skill_invoked", skill = "other", expect = false },
        ]"#,
        );
        let grades = grade_all(&list, &ws, "the final answer", &diff, &transcript, None, 1);
        let passed: Vec<bool> = grades.iter().map(|g| g.passed.unwrap()).collect();
        assert_eq!(passed, vec![true, false, true, false, true, true, true, true, false, true, false, true, true], "{grades:#?}");
        let names: std::collections::BTreeSet<_> = grades.iter().map(|g| g.grader.clone()).collect();
        assert_eq!(names.len(), grades.len(), "names must be unique");
        ws.remove();
    }

    fn graded(list: &str, transcript: &Transcript, hooks: Option<&[RunEvent]>) -> Vec<Grade> {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::init(tmp.path()).unwrap();
        let task = Task::parse("id='t'\nprompt='p'\n").unwrap();
        let ws = Workspace::create(&project, &task, &tmp.path().join("run"), "g").unwrap();
        let out = grade_all(&specs(list), &ws, "", &DiffStats::default(), transcript, hooks, 1);
        ws.remove();
        out
    }

    fn passed(g: &[Grade]) -> Vec<bool> {
        g.iter().map(|g| g.passed.unwrap()).collect()
    }

    /// Claude Code's sample stream, with the transcript the parser makes of it.
    fn sample() -> Transcript {
        crate::runner::claude::parse_stream(crate::runner::claude::SAMPLE).transcript
    }

    /// A hand-built transcript mixing both runners' tool names: read, test
    /// with Bash, edit, test with shell, final message, then a late call.
    fn two_runner() -> Transcript {
        let call = |id: &str, name: &str, input: Value| ToolCall { id: id.into(), name: name.into(), input };
        let turn = |calls: Vec<ToolCall>| Turn { role: "assistant".into(), tool_calls: calls, ..Default::default() };
        Transcript {
            turns: vec![
                Turn { role: "assistant".into(), thinking: "the unicode case is the SECRET-THOUGHT".into(), tool_calls: vec![call("c1", "Read", json!({"file_path": "slugify.py"}))], ..Default::default() },
                turn(vec![call("c2", "Bash", json!({"command": "python3 -m unittest discover -s tests"}))]),
                Turn { role: "user".into(), tool_results: vec![crate::transcript::ToolResult { tool_use_id: "c2".into(), content: "FAILED (failures=2)".into(), is_error: false }], ..Default::default() },
                turn(vec![call("c3", "Edit", json!({"file_path": "slugify.py"}))]),
                turn(vec![call("c4", "shell", json!({"command": "pytest -q"}))]),
                Turn { role: "assistant".into(), subagent: true, tool_calls: vec![call("c5", "Bash", json!({"command": "ls"}))], ..Default::default() },
                Turn { role: "assistant".into(), text: "Fixed; the tests pass.".into(), ..Default::default() },
                turn(vec![call("c6", "Bash", json!({"command": "git status"}))]),
            ],
        }
    }

    #[test]
    fn tool_call_counts_across_runners() {
        let t = two_runner();
        let g = graded(
            r#"[
            { kind = "tool_call", name = ["Bash", "shell"], min = 4, max = 4 },
            { kind = "tool_call", name = ["Bash", "shell"], input = "unittest|pytest", min = 2, max = 2, label = "tests" },
            { kind = "tool_call", name = "Bash", subagents = "exclude", min = 2, max = 2 },
            { kind = "tool_call", name = "Bash", subagents = "only", max = 1 },
            { kind = "tool_call", name = "Write", max = 0, min = 0 },
            { kind = "tool_call", name = "Write" },
            { kind = "tool_call", name = "Bash", input = "(" },
        ]"#,
            &t,
            None,
        );
        assert_eq!(passed(&g), vec![true, true, true, true, true, false, false], "{g:#?}");
        assert_eq!(g[1].grader, "tests");
        assert_eq!(g[1].detail["call_ids"], json!(["c2", "c4"]));
        assert_eq!(g[1].detail["calls"][1]["input"], "pytest -q");
        assert!(g[6].detail["error"].as_str().unwrap().contains("bad input pattern"));
        // The sample stream: one Skill call and one Write.
        let s = graded(r#"[{ kind = "tool_call", name = ["Skill", "Write"], min = 2, max = 2 }, { kind = "tool_call", name = "Wri*" }]"#, &sample(), None);
        assert_eq!(passed(&s), vec![true, true]);
    }

    #[test]
    fn sequence_respects_order_and_the_final_message() {
        let t = two_runner();
        let g = graded(
            r#"[
            { kind = "sequence", steps = [{ name = "Read" }, { name = ["Bash", "shell"], input = "unittest|pytest" }, { name = "Edit" }, { name = ["Bash", "shell"], input = "unittest|pytest" }], name = "read-test-edit-test" },
            { kind = "sequence", steps = [{ name = "Edit" }, { name = "Read" }], name = "out-of-order" },
            { kind = "sequence", steps = [{ name = "Bash", input = "git status" }], name = "after-final" },
            { kind = "sequence", steps = [{ name = "Bash", input = "git status" }], before_final = false, name = "anywhere" },
            { kind = "sequence", steps = [{ name = "Glob" }], name = "never" },
        ]"#,
            &t,
            None,
        );
        assert_eq!(passed(&g), vec![true, false, false, true, false], "{g:#?}");
        assert_eq!(g[0].detail["call_ids"], json!(["c1", "c2", "c3", "c4"]));
        assert_eq!(g[1].detail["missing_step"]["index"], 2);
        assert!(g[1].detail["missing_step"]["why"].as_str().unwrap().contains("out of order"));
        assert!(g[2].detail["missing_step"]["why"].as_str().unwrap().contains("after the final message"));
        assert_eq!(g[4].detail["missing_step"]["why"], "no matching tool call");
        // In the sample stream the skill is invoked before the file is written, both before "Done."
        let s = graded(r#"[{ kind = "sequence", steps = [{ name = "Skill" }, { name = "Write" }] }, { kind = "sequence", steps = [{ name = "Write" }, { name = "Skill" }], name = "reversed" }]"#, &sample(), None);
        assert_eq!(passed(&s), vec![true, false]);
    }

    #[test]
    fn transcript_regex_scopes() {
        let t = two_runner();
        let g = graded(
            r#"[
            { kind = "transcript_regex", pattern = "SECRET-THOUGHT", scope = "thinking" },
            { kind = "transcript_regex", pattern = "SECRET-THOUGHT", name = "assistant-does-not-see-thinking", should_match = false },
            { kind = "transcript_regex", pattern = "tests pass", scope = "thinking", should_match = false, name = "thinking-only-sees-thinking" },
            { kind = "transcript_regex", pattern = "FAILED", scope = "tool_results" },
            { kind = "transcript_regex", pattern = "FAILED", scope = "assistant", should_match = false, name = "results-not-in-assistant" },
            { kind = "transcript_regex", pattern = "tests pass" },
            { kind = "transcript_regex", pattern = "FAILED|SECRET", scope = "all" },
        ]"#,
            &t,
            None,
        );
        assert_eq!(passed(&g), vec![true, true, true, true, true, true, true], "{g:#?}");
        assert_eq!(g[5].detail["turn"], 6);
    }

    #[test]
    fn hook_fired_counts_and_refuses_to_pass_vacuously() {
        let hook = |event: &str| RunEvent { kind: "hook".into(), data: json!({"event": event, "matcher": "Bash", "exit_code": 0}), ..Default::default() };
        let events = vec![hook("PreToolUse"), hook("PreToolUse"), hook("PostToolUse")];
        let list = r#"[{ kind = "hook_fired", event = "PreToolUse", min = 2 }, { kind = "hook_fired", event = "Stop" }, { kind = "hook_fired", event = "Stop", min = 0, max = 0, name = "no-stop" }]"#;
        let g = graded(list, &sample(), Some(&events));
        assert_eq!(passed(&g), vec![true, false, true]);
        assert_eq!(g[0].detail["hooks"][0]["exit_code"], 0);
        // A runner that reports no hooks fails every hook grader, even one that expects none.
        let g = graded(list, &sample(), None);
        assert_eq!(passed(&g), vec![false, false, false]);
        assert_eq!(g[2].detail["error"], "runner does not report hooks");
    }
}

//! Deterministic graders: command exit code, file exists, regex, diff size,
//! word count, JSON schema, skill invoked.

use crate::model::DetSpec;
use crate::runner::{exec, ExecSpec};
use crate::store::Grade;
use crate::transcript::Transcript;
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

/// Run one grader. Never errors: a grader that cannot run is a failed grade
/// with the reason in its detail.
pub fn grade(spec: &DetSpec, input: &GradeInput, run_id: i64) -> Grade {
    let default_name = match spec {
        DetSpec::FileExists { path, .. } => format!("file_exists:{path}"),
        DetSpec::WordCount { path: Some(p), .. } | DetSpec::Regex { path: Some(p), .. } | DetSpec::JsonSchema { path: Some(p), .. } => {
            format!("{}:{p}", spec.kind())
        }
        DetSpec::SkillInvoked { skill: Some(s), .. } => format!("skill_invoked:{s}"),
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
    };
    Grade { run_id, grader, tier: "deterministic".into(), passed: Some(passed), score, detail }
}

/// Grade a run with every spec, keeping grader names unique.
pub fn grade_all(specs: &[DetSpec], ws: &Workspace, output: &str, diff: &DiffStats, transcript: &Transcript, run_id: i64) -> Vec<Grade> {
    let mut out: Vec<Grade> = vec![];
    for (index, spec) in specs.iter().enumerate() {
        let mut g = grade(spec, &GradeInput { ws, output, diff, transcript, index }, run_id);
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
        let grades = grade_all(&list, &ws, "the final answer", &diff, &transcript, 1);
        let passed: Vec<bool> = grades.iter().map(|g| g.passed.unwrap()).collect();
        assert_eq!(passed, vec![true, false, true, false, true, true, true, true, false, true, false, true, true], "{grades:#?}");
        let names: std::collections::BTreeSet<_> = grades.iter().map(|g| g.grader.clone()).collect();
        assert_eq!(names.len(), grades.len(), "names must be unique");
        ws.remove();
    }
}

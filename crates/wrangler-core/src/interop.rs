//! Interop with the `skill-creator` eval format: import `evals/evals.json`
//! into tasks, export results shaped for its `benchmark.json` viewer.

use crate::model::*;
use crate::project::Project;
use crate::report::Data;
use crate::stats;
use crate::store::Store;
use crate::util::slug;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
struct EvalsFile {
    #[serde(default)]
    skill_name: String,
    evals: Vec<EvalEntry>,
}

#[derive(Deserialize)]
struct EvalEntry {
    id: Value,
    prompt: String,
    #[serde(default)]
    expected_output: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    expectations: Vec<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Import a skill-creator `evals.json`. Each eval becomes
/// `tasks/<skill>/<id>.toml`; its input files become a fixture directory.
/// Returns the task files written, relative to the project root.
pub fn import_skill_creator(project: &Project, evals_json: &Path, skill_root: Option<&Path>) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(evals_json).with_context(|| format!("reading {}", evals_json.display()))?;
    let file: EvalsFile = serde_json::from_str(&text).context("parsing evals.json")?;
    // `files` are relative to the skill root: by convention the parent of `evals/`.
    let root = skill_root.map(|p| p.to_path_buf()).unwrap_or_else(|| evals_json.parent().and_then(|p| p.parent()).unwrap_or(Path::new(".")).to_path_buf());
    let skill = if file.skill_name.is_empty() { "imported".to_string() } else { slug(&file.skill_name) };
    let mut written = vec![];
    for e in &file.evals {
        let raw_id = match &e.id {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let id = format!("{skill}-{}", slug(&raw_id));
        let fixture = if e.files.is_empty() {
            None
        } else {
            let rel = format!("fixtures/{id}");
            let dst = project.abs(&rel);
            std::fs::create_dir_all(&dst)?;
            for f in &e.files {
                let src = root.join(f);
                if !src.is_file() {
                    bail!("eval {raw_id}: input file {} not found (pass --skill-root if files are relative to another directory)", src.display());
                }
                std::fs::copy(&src, dst.join(src.file_name().unwrap()))?;
            }
            Some(Fixture { dir: Some(rel), repo: None, git_ref: None })
        };
        let mut tags = vec!["imported".to_string(), skill.clone()];
        if let Some(n) = &e.name {
            tags.push(slug(n));
        }
        let task = Task {
            id: id.clone(),
            prompt: e.prompt.clone(),
            prompt_file: None,
            fixture,
            expected_output: e.expected_output.clone(),
            expectations: e.expectations.clone(),
            tags,
            output_file: None,
            ..Default::default()
        };
        let rel = format!("tasks/{skill}/{}.toml", slug(&raw_id));
        let path = project.abs(&rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(&task)?)?;
        written.push(rel);
    }
    Ok(written)
}

fn agg(xs: &[f64]) -> Value {
    if xs.is_empty() {
        return json!({"mean": 0.0, "stddev": 0.0, "min": 0.0, "max": 0.0});
    }
    json!({
        "mean": stats::mean(xs), "stddev": stats::sd(xs),
        "min": xs.iter().cloned().fold(f64::INFINITY, f64::min),
        "max": xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    })
}

/// Export a launch as skill-creator `benchmark.json`. The viewer knows two
/// configurations, so pick which variant is `with_skill` and which is
/// `without_skill` (defaults: the second and first variants).
pub fn export_benchmark(store: &Store, launch_id: i64, with: Option<&str>, without: Option<&str>) -> Result<Value> {
    let data = Data::load(store, launch_id)?;
    let variants = &data.launch.variants;
    let pick = |want: Option<&str>, default_idx: usize| -> Result<&Variant> {
        match want {
            Some(w) => variants.iter().find(|v| v.key == w || v.label == w).with_context(|| format!("no variant `{w}` (have: {})", variants.iter().map(|v| v.label.as_str()).collect::<Vec<_>>().join(", "))),
            None => variants.get(default_idx).context("launch needs at least two variants to export a benchmark"),
        }
    };
    let (v_without, v_with) = (pick(without, 0)?, pick(with, 1)?);
    let grades = store.grades_for_launch(launch_id)?;
    let mut runs_json = vec![];
    let mut summary = serde_json::Map::new();
    for (config, v) in [("with_skill", v_with), ("without_skill", v_without)] {
        let (mut rates, mut times, mut tokens) = (vec![], vec![], vec![]);
        for r in data.runs.iter().filter(|r| r.variant_key == v.key && r.status != "pending") {
            let m = data.metrics.get(&r.id).cloned().unwrap_or_default();
            let judge = grades.iter().find(|g| g.run_id == r.id && g.tier == "judge");
            let mut expectations: Vec<Value> = judge.and_then(|g| g.detail["expectations"].as_array().cloned()).unwrap_or_default();
            for g in grades.iter().filter(|g| g.run_id == r.id && g.tier == "deterministic") {
                expectations.push(json!({"text": g.grader, "passed": g.passed.unwrap_or(false), "evidence": g.detail.to_string()}));
            }
            let total = expectations.len();
            let passed = expectations.iter().filter(|e| e["passed"] == true).count();
            let pass_rate = if total > 0 { passed as f64 / total as f64 } else { 0.0 };
            let (time, toks) = (m.get("wall_time_s").copied().unwrap_or(0.0), m.get("total_tokens").copied().unwrap_or(0.0));
            rates.push(pass_rate);
            times.push(time);
            tokens.push(toks);
            let eval_id = data.launch.tasks.iter().position(|t| t.id == r.task_id).unwrap_or(0) + 1;
            runs_json.push(json!({
                "eval_id": eval_id, "eval_name": r.task_id, "configuration": config, "run_number": r.rep + 1,
                "result": {"pass_rate": pass_rate, "passed": passed, "failed": total - passed, "total": total, "time_seconds": time, "tokens": toks, "tool_calls": m.get("tool_calls").copied().unwrap_or(0.0), "errors": if r.status == "done" { 0 } else { 1 }},
                "expectations": expectations,
                "notes": if r.error.is_empty() { json!([]) } else { json!([r.error]) },
            }));
        }
        summary.insert(config.into(), json!({"pass_rate": agg(&rates), "time_seconds": agg(&times), "tokens": agg(&tokens)}));
    }
    let d = |k: &str| summary["with_skill"][k]["mean"].as_f64().unwrap_or(0.0) - summary["without_skill"][k]["mean"].as_f64().unwrap_or(0.0);
    summary.insert("delta".into(), json!({"pass_rate": format!("{:+.2}", d("pass_rate")), "time_seconds": format!("{:+.1}", d("time_seconds")), "tokens": format!("{:+.0}", d("tokens"))}));
    let judge_model = data.launch.def.judge().map(|j| j.model).unwrap_or_default();
    Ok(json!({
        "metadata": {
            "skill_name": data.launch.experiment, "skill_path": v_with.overlays().first().copied().unwrap_or(""),
            "executor_model": v_with.model.clone().unwrap_or_default(), "analyzer_model": judge_model,
            "timestamp": data.launch.created_at, "evals_run": data.launch.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            "runs_per_configuration": data.launch.def.runs_per_cell,
            "with_skill_variant": v_with.label, "without_skill_variant": v_without.label,
        },
        "runs": runs_json,
        "run_summary": summary,
        "notes": [format!("Exported from Harness Wrangler launch #{launch_id}")],
    }))
}

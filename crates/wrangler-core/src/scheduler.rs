//! Launch planning and execution: run every cell N times with bounded
//! concurrency, retry rate limits with backoff, grade, then judge.

use crate::graders::deterministic::grade_all;
use crate::graders::judge::{self, Shown};
use crate::model::*;
use crate::project::Project;
use crate::runner::{self, AgentRunner, Limits, RunEvent, RunRecord};
use crate::store::{Grade, Judgment, Launch, RunRow, Store};
use crate::util::word_count;
use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Override `[runner] concurrency`.
    pub concurrency: Option<usize>,
    /// Skip the LLM judge phase.
    pub skip_judge: bool,
    /// Also rerun runs that failed or timed out (resume).
    pub retry_failed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Started { launch_id: i64, total: usize, pending: usize, concurrency: usize },
    RunStarted { run_id: i64, task: String, variant: String, rep: u32 },
    RunFinished { run_id: i64, task: String, variant: String, rep: u32, status: String, cost_usd: Option<f64>, wall_s: f64, done: usize, total: usize, error: String },
    RateLimited { run_id: i64, wait_s: u64, attempt: u32, detail: String },
    Phase { name: String, total: usize },
    JudgeProgress { done: usize, total: usize },
    Warning { message: String },
    Finished { launch_id: i64, status: String },
}

pub type EventSink<'a> = &'a (dyn Fn(Event) + Sync);

/// Validate the experiment, resolve tasks and variants, and record a launch
/// with one pending run per cell and repetition.
pub fn create_launch(project: &Project, store: &Store, exp: &Experiment, suite: Option<&str>, note: &str) -> Result<(i64, Vec<String>)> {
    let warnings = project.validate(exp)?;
    let tasks = project.load_tasks(exp)?;
    let variants = exp.variants();
    let id = store.create_launch(exp, &tasks, &variants, suite, note)?;
    Ok((id, warnings))
}

fn backoff(attempt: u32) -> Duration {
    let base_ms = std::env::var("WRANGLER_BACKOFF_BASE_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(30_000);
    Duration::from_millis((base_ms << attempt.min(5)).min(600_000.max(base_ms)))
}

fn sleep_cancellable(d: Duration, cancel: &AtomicBool) {
    let end = Instant::now() + d;
    while Instant::now() < end && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50).min(d));
    }
}

/// Run all pending runs of a launch, then the judge phase.
pub fn execute(project: &Project, store: &Store, launch_id: i64, opts: &RunOptions, cancel: Arc<AtomicBool>, sink: EventSink) -> Result<()> {
    let launch = store.launch(launch_id)?;
    store.reset_incomplete(launch_id, opts.retry_failed)?;
    let all = store.runs(launch_id)?;
    let total = all.len();
    let pending: VecDeque<RunRow> = all.iter().filter(|r| r.status == "pending").cloned().collect();
    let concurrency = opts.concurrency.unwrap_or(launch.def.runner.concurrency).clamp(1, 16);
    let mut runners: HashMap<String, Arc<dyn AgentRunner>> = HashMap::new();
    for v in &launch.variants {
        if !runners.contains_key(&v.runner) {
            runners.insert(v.runner.clone(), runner::get(&v.runner)?);
        }
    }
    store.set_launch_status(launch_id, "running", "runs")?;
    sink(Event::Started { launch_id, total, pending: pending.len(), concurrency });

    let done = AtomicUsize::new(total - pending.len());
    let queue = Mutex::new(pending);
    let outcome: Result<()> = std::thread::scope(|scope| {
        let mut handles = vec![];
        for _ in 0..concurrency {
            handles.push(scope.spawn(|| -> Result<()> {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let Some(run) = queue.lock().unwrap().pop_front() else { return Ok(()) };
                    let task = launch.tasks.iter().find(|t| t.id == run.task_id).context("run references unknown task")?;
                    let variant = launch.variants.iter().find(|v| v.key == run.variant_key).context("run references unknown variant")?;
                    sink(Event::RunStarted { run_id: run.id, task: task.id.clone(), variant: variant.label.clone(), rep: run.rep });
                    let started = Instant::now();
                    let res = execute_run(project, store, &launch, &run, task, variant, runners[&variant.runner].as_ref(), &cancel, sink);
                    let (status, cost, error) = match res {
                        Ok(v) => v,
                        Err(e) => {
                            let msg = format!("{e:#}");
                            store.finish_run(run.id, "failed", None, started.elapsed().as_millis() as i64, "", "", &msg, &json!([]), &[], "", &BTreeMap::new())?;
                            ("failed".to_string(), None, msg)
                        }
                    };
                    sink(Event::RunFinished {
                        run_id: run.id,
                        task: task.id.clone(),
                        variant: variant.label.clone(),
                        rep: run.rep,
                        status,
                        cost_usd: cost,
                        wall_s: started.elapsed().as_secs_f64(),
                        done: done.fetch_add(1, Ordering::Relaxed) + 1,
                        total,
                        error,
                    });
                }
            }));
        }
        let mut first_err = Ok(());
        for h in handles {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => first_err = Err(e),
                Err(_) => first_err = Err(anyhow::anyhow!("a worker thread panicked")),
            }
        }
        first_err
    });
    if let Err(e) = outcome {
        store.set_launch_status(launch_id, "failed", "")?;
        sink(Event::Finished { launch_id, status: "failed".into() });
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        for r in store.runs(launch_id)?.iter().filter(|r| r.status == "running") {
            store.set_run_status(r.id, "cancelled", "")?;
        }
        store.set_launch_status(launch_id, "cancelled", "")?;
        sink(Event::Finished { launch_id, status: "cancelled".into() });
        return Ok(());
    }
    if !opts.skip_judge {
        if let Err(e) = judge_launch(project, store, launch_id, concurrency, &cancel, sink) {
            sink(Event::Warning { message: format!("judge phase failed: {e:#}") });
        }
    }
    let status = if cancel.load(Ordering::Relaxed) { "cancelled" } else { "done" };
    store.set_launch_status(launch_id, status, "")?;
    sink(Event::Finished { launch_id, status: status.into() });
    Ok(())
}

fn overlay_skill_names(ws: &Workspace) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(ws.dir.join(".claude/skills"))
        .map(|rd| rd.flatten().filter(|e| e.path().join("SKILL.md").exists()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Skill names as they appear in frontmatter may differ from directory
/// names; accept either.
fn skill_aliases(ws: &Workspace, dir_name: &str) -> Vec<String> {
    let mut names = vec![dir_name.to_string()];
    if let Ok(text) = std::fs::read_to_string(ws.dir.join(".claude/skills").join(dir_name).join("SKILL.md")) {
        if let Some(n) = crate::ablation::frontmatter_field(&text, "name") {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

#[allow(clippy::too_many_arguments)]
fn execute_run(
    project: &Project,
    store: &Store,
    launch: &Launch,
    run: &RunRow,
    task: &Task,
    variant: &Variant,
    runner: &dyn AgentRunner,
    cancel: &Arc<AtomicBool>,
    sink: EventSink,
) -> Result<(String, Option<f64>, String)> {
    let exp = &launch.def;
    let run_dir = project.runs_dir().join(launch.id.to_string()).join(run.id.to_string());
    let _ = std::fs::remove_dir_all(&run_dir);
    let ws = Workspace::create(project, task, &run_dir, &format!("l{}-r{}", launch.id, run.id))?;
    let result = (|| -> Result<(String, Option<f64>, String)> {
        runner.materialize(&ws, variant)?;
        let overlay_skills = overlay_skill_names(&ws);
        let skill_names: Vec<String> = overlay_skills.iter().flat_map(|s| skill_aliases(&ws, s)).collect();
        ws.commit_baseline()?;
        store.mark_run_started(run.id, &run_dir.to_string_lossy())?;
        let limits = Limits {
            timeout: Duration::from_secs(task.timeout_secs.unwrap_or(exp.timeout_secs)),
            cancel: cancel.clone(),
            isolation: exp.isolation,
            network: exp.network,
            cfg: exp.runner.clone(),
        };
        let started = Instant::now();
        let mut events: Vec<RunEvent> = vec![];
        let mut attempt = 0u32;
        let mut rec: RunRecord = loop {
            let mut rec = runner.run(&ws, task, variant, &limits)?;
            let offset = started.elapsed().as_millis() as u64;
            for mut e in rec.events.drain(..) {
                e.at_ms = offset;
                events.push(e);
            }
            match &rec.rate_limited {
                Some(detail) if attempt < exp.runner.max_retries && !cancel.load(Ordering::Relaxed) => {
                    let wait = backoff(attempt);
                    attempt += 1;
                    events.push(RunEvent { kind: "rate_limit".into(), at_ms: offset, detail: detail.clone() });
                    events.push(RunEvent { kind: "retry".into(), at_ms: offset, detail: format!("attempt {attempt} after {}s backoff", wait.as_secs()) });
                    sink(Event::RateLimited { run_id: run.id, wait_s: wait.as_secs(), attempt, detail: detail.clone() });
                    sleep_cancellable(wait, cancel);
                    ws.reset_to_baseline()?;
                }
                _ => break rec,
            }
        };
        let status = if rec.cancelled {
            "cancelled"
        } else if rec.timed_out {
            events.push(RunEvent { kind: "timeout".into(), at_ms: rec.wall_ms, detail: format!("killed after {}s", limits.timeout.as_secs()) });
            "timeout"
        } else if rec.error.is_some() {
            "failed"
        } else {
            "done"
        };
        let (_patch, diff) = ws.snapshot()?;
        let file_output = task.output_file.as_ref().and_then(|f| ws.read_file(f));
        let output_text = file_output.clone().unwrap_or_else(|| rec.final_text.clone());
        let skills = rec.transcript.skills_invoked();

        let m = &mut rec.metrics;
        m.insert("diff_added".into(), diff.added as f64);
        m.insert("diff_removed".into(), diff.removed as f64);
        m.insert("diff_lines".into(), (diff.added + diff.removed) as f64);
        m.insert("files_touched".into(), diff.files.len() as f64);
        m.insert("output_words".into(), word_count(&output_text) as f64);
        m.insert("retries".into(), attempt as f64);
        // Wall time of the whole run including backoff waits is in runs.wall_ms;
        // the metric stays the agent's own time so rate limits do not skew it.
        if !overlay_skills.is_empty() {
            let hit = skills.iter().any(|s| skill_names.contains(s));
            m.insert("skill_invoked".into(), if hit { 1.0 } else { 0.0 });
        }

        std::fs::write(run_dir.join("transcript.json"), serde_json::to_string(&rec.transcript)?)?;
        if !run_dir.join("raw.jsonl").exists() {
            std::fs::write(run_dir.join("raw.jsonl"), &rec.raw_output)?;
        }
        std::fs::write(run_dir.join("final.txt"), &rec.final_text)?;
        std::fs::write(
            run_dir.join("meta.json"),
            serde_json::to_string_pretty(&json!({"command": rec.command, "diff": diff, "output_file": task.output_file, "output_is_file": file_output.is_some()}))?,
        )?;
        // Keep the files the agent touched so reviewers can open them later.
        for f in diff.files.iter().take(200) {
            if let Some(src) = ws.resolve(f) {
                if src.is_file() && src.metadata().map(|m| m.len() < 1_000_000).unwrap_or(false) {
                    let dst = run_dir.join("files").join(f);
                    if let Some(p) = dst.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let _ = std::fs::copy(&src, &dst);
                }
            }
        }

        let mut grades: Vec<Grade> = vec![];
        if status == "done" || status == "timeout" {
            let mut specs = exp.default_graders().deterministic;
            specs.extend(task.graders.deterministic.iter().cloned());
            grades = grade_all(&specs, &ws, &output_text, &diff, &rec.transcript, run.id);
            if !grades.is_empty() {
                let all = grades.iter().all(|g| g.passed == Some(true));
                rec.metrics.insert("pass".into(), if all { 1.0 } else { 0.0 });
                let frac = grades.iter().filter(|g| g.passed == Some(true)).count() as f64 / grades.len() as f64;
                rec.metrics.insert("checks_passed".into(), frac);
            }
        }
        let error = match (&rec.error, status) {
            (Some(e), _) => e.clone(),
            (None, "timeout") => format!("timed out after {}s", limits.timeout.as_secs()),
            _ => String::new(),
        };
        store.finish_run(
            run.id,
            status,
            rec.exit_status,
            started.elapsed().as_millis() as i64,
            &rec.final_text,
            &output_text,
            &error,
            &serde_json::to_value(&events)?,
            &skills,
            &rec.model,
            &rec.metrics,
        )?;
        for g in &grades {
            store.put_grade(g)?;
        }
        Ok((status.to_string(), rec.metrics.get("cost_usd").copied(), error))
    })();
    if exp.keep_workspaces {
        let _ = std::fs::write(run_dir.join("workspace-path.txt"), ws.dir.to_string_lossy().as_bytes());
    } else {
        ws.remove();
    }
    result
}

// ------------------------------------------------------------------ judge

/// Drop one file's section from a unified diff.
pub fn diff_without(patch: &str, file: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            skipping = line.trim_end().ends_with(&format!(" b/{file}"));
        }
        if !skipping {
            out.push_str(line);
        }
    }
    out
}

/// What judges and reviewers see for a run: the output, and the diff of
/// everything else the agent changed.
pub fn shown_for(run: &RunRow, task: &Task) -> Shown {
    let patch = std::fs::read_to_string(std::path::Path::new(&run.dir).join("diff.patch")).unwrap_or_default();
    let meta: serde_json::Value = std::fs::read_to_string(std::path::Path::new(&run.dir).join("meta.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let output_is_file = meta["output_is_file"].as_bool().unwrap_or(false);
    let diff = match (&task.output_file, output_is_file) {
        (Some(f), true) => diff_without(&patch, f),
        _ => patch,
    };
    Shown { output: run.output_text.clone(), diff, output_is_file }
}

/// Run pairs the pairwise judge compares: same task, same repetition,
/// different variants.
pub fn judge_pairs(launch: &Launch, runs: &[RunRow], cfg: &JudgeCfg) -> Vec<(String, i64, i64)> {
    let nv = launch.variants.len();
    let mode = cfg.pairwise_mode.clone().unwrap_or_else(|| if nv <= 4 { "all".into() } else { "baseline".into() });
    let reps = cfg.pairwise_reps.unwrap_or(launch.def.runs_per_cell.min(3));
    let find = |task: &str, key: &str, rep: u32| runs.iter().find(|r| r.task_id == task && r.variant_key == key && r.rep == rep && r.is_complete()).map(|r| r.id);
    let mut out = vec![];
    for t in &launch.tasks {
        if t.graders.judge == Some(false) {
            continue;
        }
        for i in 0..nv {
            for j in (i + 1)..nv {
                if mode == "baseline" && i != 0 {
                    continue;
                }
                for rep in 0..reps {
                    if let (Some(a), Some(b)) = (find(&t.id, &launch.variants[i].key, rep), find(&t.id, &launch.variants[j].key, rep)) {
                        out.push((t.id.clone(), a, b));
                    }
                }
            }
        }
    }
    out
}

fn parallel_for<T: Sync>(items: &[T], concurrency: usize, cancel: &AtomicBool, f: &(dyn Fn(&T) + Sync)) {
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..concurrency.max(1).min(items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() || cancel.load(Ordering::Relaxed) {
                    break;
                }
                f(&items[i]);
            });
        }
    });
}

/// Rubric-score every completed run that has no judge grade yet, then run
/// the pairwise comparisons that are still missing. Safe to call again.
pub fn judge_launch(project: &Project, store: &Store, launch_id: i64, concurrency: usize, cancel: &Arc<AtomicBool>, sink: EventSink) -> Result<()> {
    let launch = store.launch(launch_id)?;
    let Some(cfg) = launch.def.judge() else { return Ok(()) };
    let rubric = match &cfg.rubric {
        Some(p) => Some(std::fs::read_to_string(project.abs(p)).with_context(|| format!("reading rubric {p}"))?),
        None => None,
    };
    let backend = judge::backend_for(&cfg);
    let runs = store.runs(launch_id)?;
    let task_of = |id: &str| launch.tasks.iter().find(|t| t.id == id);
    let errors = Mutex::new(Vec::<String>::new());

    if cfg.rubric_scoring {
        let graded: std::collections::HashSet<i64> = store.grades_for_launch(launch_id)?.iter().filter(|g| g.tier == "judge").map(|g| g.run_id).collect();
        let todo: Vec<&RunRow> = runs
            .iter()
            .filter(|r| r.is_complete() && !graded.contains(&r.id) && task_of(&r.task_id).map(|t| t.graders.judge != Some(false)).unwrap_or(false))
            .collect();
        if !todo.is_empty() {
            store.set_launch_status(launch_id, "running", "judging")?;
            sink(Event::Phase { name: "judge".into(), total: todo.len() });
            let done = AtomicUsize::new(0);
            parallel_for(&todo, concurrency, cancel, &|run: &&RunRow| {
                let task = task_of(&run.task_id).unwrap();
                match judge::score_run(backend.as_ref(), &cfg, task, rubric.as_deref(), &shown_for(run, task), cancel) {
                    Ok(r) => {
                        let frac = r.pass_fraction();
                        let _ = store.put_grade(&Grade {
                            run_id: run.id,
                            grader: "judge".into(),
                            tier: "judge".into(),
                            passed: frac.map(|f| f >= 1.0),
                            score: Some(r.score),
                            detail: json!({"expectations": r.expectations, "summary": r.summary, "model": cfg.model, "effort": cfg.effort, "rubric": cfg.rubric}),
                        });
                        let _ = store.set_metric(run.id, "judge_score", r.score);
                        if let Some(f) = frac {
                            let _ = store.set_metric(run.id, "expectations_passed", f);
                        }
                        let _ = store.set_metric(run.id, "judge_cost_usd", r.cost_usd);
                    }
                    Err(e) => errors.lock().unwrap().push(format!("run {}: {e:#}", run.id)),
                }
                sink(Event::JudgeProgress { done: done.fetch_add(1, Ordering::Relaxed) + 1, total: todo.len() });
            });
        }
    }

    if cfg.pairwise && !cancel.load(Ordering::Relaxed) {
        let existing: std::collections::HashSet<(i64, i64)> = store.judgments(launch_id, Some("judge"))?.iter().map(|j| (j.run_a, j.run_b)).collect();
        let pairs: Vec<(String, i64, i64)> = judge_pairs(&launch, &runs, &cfg).into_iter().filter(|(_, a, b)| !existing.contains(&(*a, *b))).collect();
        if !pairs.is_empty() {
            store.set_launch_status(launch_id, "running", "pairwise")?;
            sink(Event::Phase { name: "pairwise".into(), total: pairs.len() });
            let done = AtomicUsize::new(0);
            let by_id: HashMap<i64, &RunRow> = runs.iter().map(|r| (r.id, r)).collect();
            parallel_for(&pairs, concurrency, cancel, &|(task_id, a, b): &(String, i64, i64)| {
                let task = task_of(task_id).unwrap();
                match judge::compare(backend.as_ref(), &cfg, task, rubric.as_deref(), &shown_for(by_id[a], task), &shown_for(by_id[b], task), cancel) {
                    Ok(r) => {
                        let _ = store.add_judgment(&Judgment {
                            id: 0,
                            launch_id,
                            task_id: task_id.clone(),
                            run_a: *a,
                            run_b: *b,
                            source: "judge".into(),
                            winner: r.winner.clone(),
                            reason: r.first_reason.clone(),
                            consistent: Some(r.consistent),
                            detail: json!({"first_pick": r.first_pick, "second_pick": r.second_pick, "first_reason": r.first_reason, "second_reason": r.second_reason, "cost_usd": r.cost_usd, "model": cfg.model}),
                            created_at: String::new(),
                        });
                    }
                    Err(e) => errors.lock().unwrap().push(format!("pair {a} vs {b}: {e:#}")),
                }
                sink(Event::JudgeProgress { done: done.fetch_add(1, Ordering::Relaxed) + 1, total: pairs.len() });
            });
        }
    }
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        bail!("{} judge call(s) failed; rerun `wrangler judge {launch_id}` to fill them in. First: {}", errors.len(), errors[0]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_output_file_from_diff() {
        let patch = "diff --git a/SUMMARY.md b/SUMMARY.md\nnew file\n+hello\ndiff --git a/src/x.rs b/src/x.rs\n+code\n";
        let d = diff_without(patch, "SUMMARY.md");
        assert!(!d.contains("hello") && d.contains("+code"));
    }
}

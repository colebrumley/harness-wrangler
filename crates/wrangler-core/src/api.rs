//! One JSON command surface over the core, shared by the desktop app (Tauri
//! `invoke`) and `wrangler serve` (HTTP). Commands take and return JSON;
//! long work runs on background threads and is observed by polling.

use crate::ablation::{self, AblationRequest};
use crate::project::{Project, Suite};
use crate::scheduler::{self, Event, RunOptions};
use crate::store::{Annotation, Store};
use crate::{advice, interop, report, review, runner, suite};
use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Api {
    pub project: Project,
    pub store: Arc<Store>,
    /// Cancel flags of launches running in this process.
    jobs: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// Recent scheduler events per launch.
    events: Mutex<HashMap<i64, VecDeque<Event>>>,
    /// Suite runs in progress: suite name → (model, cancel flag, error).
    suite_jobs: Mutex<HashMap<String, SuiteJob>>,
}

#[derive(Clone)]
struct SuiteJob {
    model: String,
    cancel: Arc<AtomicBool>,
    running: bool,
    error: Option<String>,
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null)).map_err(|e| anyhow!("argument `{key}`: {e}"))
}

fn opt<T: DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|e| anyhow!("argument `{key}`: {e}")),
    }
}

pub const EXPERIMENT_TEMPLATE: &str = r#"name = "my-experiment"
description = "What question does this answer?"
runs_per_cell = 5
timeout_secs = 900
isolation = "workspace"   # or "seatbelt"

[runner]
kind = "claude-code"
concurrency = 3
model = "claude-sonnet-5-5"

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none"]

[tasks]
include = ["tasks/**/*.toml"]

[graders.default]
judge = { model = "claude-opus-5-5", pairwise = true }
human = { enabled = true }
"#;

impl Api {
    pub fn new(project: Project) -> Result<Arc<Api>> {
        let store = Arc::new(Store::open(&project.db_path())?);
        store.mark_stale()?;
        Ok(Arc::new(Api { project, store, jobs: Mutex::new(HashMap::new()), events: Mutex::new(HashMap::new()), suite_jobs: Mutex::new(HashMap::new()) }))
    }

    fn record(&self, launch_id: i64, e: Event) {
        let mut ev = self.events.lock().unwrap();
        let q = ev.entry(launch_id).or_default();
        q.push_back(e);
        while q.len() > 60 {
            q.pop_front();
        }
    }

    fn resolve(&self, args: &Value) -> Result<i64> {
        if let Some(id) = opt::<i64>(args, "launch_id")? {
            return Ok(id);
        }
        let exp: String = arg(args, "experiment").context("need `launch_id` or `experiment`")?;
        self.store.resolve_launch(&exp)
    }

    /// Run a launch (or resume one) on a background thread.
    fn spawn_launch(self: &Arc<Self>, launch_id: i64, opts: RunOptions) -> Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.jobs.lock().unwrap();
            if jobs.contains_key(&launch_id) {
                bail!("launch {launch_id} is already running");
            }
            jobs.insert(launch_id, cancel.clone());
        }
        self.store.set_launch_status(launch_id, "running", "starting")?;
        let me = self.clone();
        std::thread::spawn(move || {
            let res = scheduler::execute(&me.project, &me.store, launch_id, &opts, cancel, &|e| me.record(launch_id, e));
            if let Err(e) = res {
                me.record(launch_id, Event::Warning { message: format!("{e:#}") });
                let _ = me.store.set_launch_status(launch_id, "failed", "");
            }
            me.jobs.lock().unwrap().remove(&launch_id);
        });
        Ok(())
    }

    pub fn dispatch(self: &Arc<Self>, cmd: &str, args: &Value) -> Result<Value> {
        let (p, s) = (&self.project, &self.store);
        Ok(match cmd {
            "project_info" => json!({
                "root": p.root, "data_dir": p.data_dir, "name": p.root.file_name().map(|n| n.to_string_lossy().to_string()),
                "runners": runner::known(), "version": env!("CARGO_PKG_VERSION"),
            }),

            // ------------------------------------------------ experiments
            "experiments_list" => {
                let launches = s.list_launches(None)?;
                let list: Vec<Value> = p
                    .list_experiments()
                    .into_iter()
                    .map(|e| {
                        let mine: Vec<_> = launches.iter().filter(|l| l.experiment == e.name).collect();
                        let mut v = serde_json::to_value(&e).unwrap();
                        v["launches"] = json!(mine.len());
                        v["last_launch"] = json!(mine.first());
                        v
                    })
                    .collect();
                json!(list)
            }
            "experiment_get" => {
                let name: String = arg(args, "name")?;
                let toml_text = p.experiment_toml(&name)?;
                let mut out = json!({"name": name, "toml": toml_text, "launches": s.list_launches(Some(&name))?});
                match p.load_experiment(&name) {
                    Ok(exp) => {
                        out["def"] = json!(exp);
                        out["variants"] = json!(exp.variants());
                        match p.validate(&exp) {
                            Ok(w) => out["warnings"] = json!(w),
                            Err(e) => out["error"] = json!(format!("{e:#}")),
                        }
                        if let Ok(tasks) = p.load_tasks(&exp) {
                            out["total_runs"] = json!(tasks.len() * exp.variants().len() * exp.runs_per_cell as usize);
                            out["tasks"] = json!(tasks);
                        }
                    }
                    Err(e) => out["error"] = json!(format!("{e:#}")),
                }
                out
            }
            "experiment_validate" => {
                let text: String = arg(args, "toml")?;
                match crate::model::Experiment::parse(&text) {
                    Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
                    Ok(exp) => {
                        let variants = exp.variants();
                        let tasks = p.load_tasks(&exp).map(|t| t.len()).unwrap_or(0);
                        match p.validate(&exp) {
                            Ok(w) => json!({"ok": true, "warnings": w, "def": exp, "variants": variants, "tasks": tasks, "total_runs": tasks * variants.len() * exp.runs_per_cell as usize}),
                            Err(e) => json!({"ok": false, "error": format!("{e:#}"), "def": exp, "variants": variants, "tasks": tasks}),
                        }
                    }
                }
            }
            "experiment_save" => {
                let (name, text): (String, String) = (arg(args, "name")?, arg(args, "toml")?);
                let exp = p.save_experiment_toml(&name, &text)?;
                json!({"ok": true, "name": name, "def": exp})
            }
            "experiment_render" => {
                // Structured editor → TOML.
                let exp: crate::model::Experiment = arg(args, "def")?;
                json!({"toml": exp.to_toml()?})
            }
            "experiment_delete" => {
                let name: String = arg(args, "name")?;
                std::fs::remove_file(p.experiment_path(&name))?;
                json!({"ok": true})
            }
            "experiment_template" => json!({"toml": EXPERIMENT_TEMPLATE}),
            "tasks_list" => {
                let list: Vec<Value> = p
                    .list_all_tasks()
                    .into_iter()
                    .map(|t| match t {
                        Ok(t) => json!({"id": t.id, "source": t.source, "tags": t.tags, "prompt": t.prompt, "fixture": t.fixture, "expectations": t.expectations.len(), "output_file": t.output_file, "deterministic": t.graders.deterministic.len()}),
                        Err(e) => json!({"error": format!("{e:#}")}),
                    })
                    .collect();
                json!(list)
            }
            "task_get" => {
                let source: String = arg(args, "source")?;
                json!({"source": source, "toml": std::fs::read_to_string(self.task_path(&source)?)?})
            }
            "task_save" => {
                let (source, text): (String, String) = (arg(args, "source")?, arg(args, "toml")?);
                crate::model::Task::parse(&text)?;
                let path = self.task_path(&source)?;
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(path, text)?;
                json!({"ok": true})
            }
            "overlays_list" => json!(p.list_overlays()),

            // --------------------------------------------------- launches
            "launch_start" => {
                let name: String = arg(args, "experiment")?;
                let exp = p.load_experiment(&name)?;
                let (id, warnings) = scheduler::create_launch(p, s, &exp, None, &opt::<String>(args, "note")?.unwrap_or_default())?;
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: opt(args, "skip_judge")?.unwrap_or(false), retry_failed: false })?;
                json!({"launch_id": id, "warnings": warnings})
            }
            "launch_resume" => {
                let id: i64 = arg(args, "launch_id")?;
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: opt(args, "skip_judge")?.unwrap_or(false), retry_failed: opt(args, "retry_failed")?.unwrap_or(true) })?;
                json!({"launch_id": id})
            }
            "launch_cancel" => {
                let id: i64 = arg(args, "launch_id")?;
                match self.jobs.lock().unwrap().get(&id) {
                    Some(c) => c.store(true, Ordering::Relaxed),
                    None => bail!("launch {id} is not running in this process"),
                }
                json!({"ok": true})
            }
            "launch_delete" => {
                let id: i64 = arg(args, "launch_id")?;
                if self.jobs.lock().unwrap().contains_key(&id) {
                    bail!("launch {id} is running; cancel it first");
                }
                s.delete_launch(id)?;
                let dir = p.runs_dir().join(id.to_string());
                let _ = std::fs::remove_dir_all(dir);
                json!({"ok": true})
            }
            "launches_list" => json!(s.list_launches(opt::<String>(args, "experiment")?.as_deref())?),
            "launch_status" => self.launch_status(self.resolve(args)?)?,
            "judge_run" => {
                // Fill in missing judge grades and pairwise comparisons.
                let id: i64 = self.resolve(args)?;
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: false, retry_failed: false })?;
                json!({"launch_id": id})
            }

            // ---------------------------------------------------- results
            "report" => json!(report::build(s, self.resolve(args)?, opt::<String>(args, "metric")?.as_deref())?),
            "runs_list" => {
                let id = self.resolve(args)?;
                let launch = s.launch(id)?;
                let metrics = s.metrics_for_launch(id)?;
                let rows: Vec<Value> = s
                    .runs(id)?
                    .iter()
                    .map(|r| {
                        let m = metrics.get(&r.id);
                        let g = |k: &str| m.and_then(|m| m.get(k)).copied();
                        json!({
                            "id": r.id, "task_id": r.task_id, "variant_key": r.variant_key,
                            "variant": launch.variants.iter().find(|v| v.key == r.variant_key).map(|v| v.label.clone()),
                            "rep": r.rep, "status": r.status, "error": r.error, "model": r.model,
                            "cost_usd": g("cost_usd"), "wall_time_s": g("wall_time_s"), "judge_score": g("judge_score"), "pass": g("pass"),
                            "turns": g("turns"), "total_tokens": g("total_tokens"), "skill_invoked": g("skill_invoked"),
                        })
                    })
                    .collect();
                json!(rows)
            }
            "run_get" => self.run_get(arg(args, "run_id")?)?,

            // ----------------------------------------------------- review
            "review_next" => {
                let id = self.resolve(args)?;
                json!({"item": review::next(s, id)?, "progress": review::progress(s, id)?})
            }
            "review_vote" => json!(review::vote(s, arg(args, "launch_id")?, arg(args, "left_run")?, arg(args, "right_run")?, &arg::<String>(args, "choice")?, &opt::<String>(args, "reason")?.unwrap_or_default())?),
            "review_undo" => {
                s.delete_judgment(arg(args, "judgment_id")?)?;
                json!({"ok": true})
            }
            "annotation_add" => {
                let a: Annotation = serde_json::from_value(args.clone()).map_err(|e| anyhow!("annotation: {e}"))?;
                if !matches!(a.kind.as_str(), "comment" | "emoji") || !matches!(a.target.as_str(), "output" | "diff") {
                    bail!("annotation kind must be comment|emoji and target output|diff");
                }
                let id = s.add_annotation(&a)?;
                json!({"id": id, "annotations": s.annotations_for_run(a.run_id)?})
            }
            "annotation_delete" => {
                s.delete_annotation(arg(args, "id")?)?;
                json!({"ok": true})
            }
            "annotations_digest" => json!(review::annotation_digest(s, self.resolve(args)?)?),

            // --------------------------------------------------- ablation
            "ablation_preview" => {
                let source: String = arg(args, "source")?;
                let path = p.abs(&source);
                let file = if path.is_dir() {
                    let skills: Vec<_> = std::fs::read_dir(path.join(".claude/skills")).map(|rd| rd.flatten().map(|e| e.path().join("SKILL.md")).filter(|f| f.is_file()).collect()).unwrap_or_default();
                    if path.join("SKILL.md").is_file() {
                        path.join("SKILL.md")
                    } else if skills.len() == 1 {
                        skills[0].clone()
                    } else {
                        path.join("CLAUDE.md")
                    }
                } else {
                    path
                };
                let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
                json!({"file": p.rel(&file), "sections": ablation::split_sections(&text, opt(args, "depth")?.unwrap_or(2)), "est_tokens": crate::util::est_tokens(&text)})
            }
            "ablation_generate" => {
                let req: AblationRequest = serde_json::from_value(args.clone()).map_err(|e| anyhow!("ablation request: {e}"))?;
                json!(ablation::generate(p, &req)?)
            }

            // ------------------------------------------------- regression
            "suites_list" => {
                let jobs = self.suite_jobs.lock().unwrap().clone();
                let list: Vec<Value> = p
                    .list_suites()
                    .into_iter()
                    .map(|su| {
                        let models: Vec<String> = s.baselines(&su.name).map(|b| b.into_iter().map(|b| b.model).fold(vec![], |mut acc, m| {
                            if !acc.contains(&m) {
                                acc.push(m)
                            }
                            acc
                        })).unwrap_or_default();
                        json!({"suite": su, "models": models, "running": jobs.get(&su.name).map(|j| j.running).unwrap_or(false)})
                    })
                    .collect();
                json!(list)
            }
            "suite_get" => {
                let name: String = arg(args, "name")?;
                let job = self.suite_jobs.lock().unwrap().get(&name).cloned();
                let active: Vec<_> = s.list_launches(None)?.into_iter().filter(|l| l.suite.as_deref() == Some(name.as_str()) && l.status == "running").collect();
                json!({
                    "dashboard": suite::dashboard(p, s, &name)?,
                    "job": job.map(|j| json!({"model": j.model, "running": j.running, "error": j.error})),
                    "active_launches": active,
                })
            }
            "suite_save" => {
                let su: Suite = arg(args, "suite")?;
                p.save_suite(&su)?;
                json!({"ok": true})
            }
            "suite_delete" => {
                std::fs::remove_file(p.suite_path(&arg::<String>(args, "name")?))?;
                json!({"ok": true})
            }
            "suite_run" => {
                let (name, model): (String, String) = (arg(args, "name")?, arg(args, "model")?);
                let cancel = Arc::new(AtomicBool::new(false));
                {
                    let mut jobs = self.suite_jobs.lock().unwrap();
                    if jobs.get(&name).map(|j| j.running).unwrap_or(false) {
                        bail!("suite {name} is already running");
                    }
                    // Fail fast on a bad suite before going to the background.
                    for e in &p.load_suite(&name)?.experiments {
                        p.validate(&suite::pin_model(&p.load_experiment(e)?, &model)?)?;
                    }
                    jobs.insert(name.clone(), SuiteJob { model: model.clone(), cancel: cancel.clone(), running: true, error: None });
                }
                let me = self.clone();
                let opts = RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: false, retry_failed: false };
                std::thread::spawn(move || {
                    let res = suite::run_model(&me.project, &me.store, &name, &model, &opts, cancel, &|e| {
                        if let Event::Started { launch_id, .. } | Event::Finished { launch_id, .. } = &e {
                            me.record(*launch_id, e.clone());
                        }
                    });
                    if let Some(j) = me.suite_jobs.lock().unwrap().get_mut(&name) {
                        j.running = false;
                        j.error = res.err().map(|e| format!("{e:#}"));
                    }
                });
                json!({"ok": true})
            }
            "suite_cancel" => {
                match self.suite_jobs.lock().unwrap().get(&arg::<String>(args, "name")?) {
                    Some(j) => j.cancel.store(true, Ordering::Relaxed),
                    None => bail!("suite is not running"),
                }
                json!({"ok": true})
            }
            "suite_diff" => json!(suite::diff(p, s, &arg::<String>(args, "name")?, &arg::<String>(args, "model")?, opt::<String>(args, "previous")?.as_deref())?),

            // ----------------------------------------------------- advice
            "advice" => {
                let id = self.resolve(args)?;
                let metric = opt::<String>(args, "metric")?;
                let rep = report::build(s, id, metric.as_deref())?;
                json!({"launch_id": id, "experiment": rep.experiment, "primary_metric": rep.primary_metric, "suggestions": advice::build(s, id, metric.as_deref())?})
            }
            "export_benchmark" => interop::export_benchmark(s, self.resolve(args)?, opt::<String>(args, "with")?.as_deref(), opt::<String>(args, "without")?.as_deref())?,

            other => bail!("unknown command `{other}`"),
        })
    }

    fn task_path(&self, source: &str) -> Result<std::path::PathBuf> {
        if !source.starts_with("tasks/") || !source.ends_with(".toml") || source.contains("..") {
            bail!("task files live under tasks/ and end in .toml");
        }
        Ok(self.project.abs(source))
    }

    fn launch_status(&self, id: i64) -> Result<Value> {
        let launch = self.store.launch(id)?;
        let runs = self.store.runs(id)?;
        let metrics = self.store.metrics_for_launch(id)?;
        let mut cells = vec![];
        for t in &launch.tasks {
            for v in &launch.variants {
                let mine: Vec<_> = runs.iter().filter(|r| r.task_id == t.id && r.variant_key == v.key).collect();
                let cost: f64 = mine.iter().filter_map(|r| metrics.get(&r.id)?.get("cost_usd")).sum();
                cells.push(json!({
                    "task_id": t.id, "variant_key": v.key,
                    "runs": mine.iter().map(|r| json!({"id": r.id, "rep": r.rep, "status": r.status})).collect::<Vec<_>>(),
                    "cost_usd": cost,
                }));
            }
        }
        let count = |st: &str| runs.iter().filter(|r| r.status == st).count();
        let sum = |k: &str| metrics.values().filter_map(|m| m.get(k)).sum::<f64>();
        let judged = self.store.grades_for_launch(id)?.iter().filter(|g| g.tier == "judge").count();
        Ok(json!({
            "launch_id": id, "experiment": launch.experiment, "status": launch.status, "phase": launch.phase,
            "created_at": launch.created_at, "finished_at": launch.finished_at, "note": launch.note, "suite": launch.suite,
            "active": self.jobs.lock().unwrap().contains_key(&id),
            "counts": {"total": runs.len(), "done": count("done"), "failed": count("failed"), "timeout": count("timeout"), "running": count("running"), "pending": count("pending"), "cancelled": count("cancelled")},
            "cost_usd": sum("cost_usd"), "judge_cost_usd": sum("judge_cost_usd"),
            "judged": judged, "judgments": self.store.judgments(id, Some("judge"))?.len(),
            "tasks": launch.tasks.iter().map(|t| &t.id).collect::<Vec<_>>(),
            "variants": launch.variants.iter().map(|v| json!({"key": v.key, "label": v.label})).collect::<Vec<_>>(),
            "runs_per_cell": launch.def.runs_per_cell,
            "has_judge": launch.def.judge().is_some(), "human_review": launch.def.human_enabled(),
            "cells": cells,
            "events": self.events.lock().unwrap().get(&id).map(|q| q.iter().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        }))
    }

    fn run_get(&self, run_id: i64) -> Result<Value> {
        let run = self.store.run(run_id)?;
        let launch = self.store.launch(run.launch_id)?;
        let dir = std::path::Path::new(&run.dir);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        let transcript: Value = serde_json::from_str(&read("transcript.json")).unwrap_or(json!({"turns": []}));
        let meta: Value = serde_json::from_str(&read("meta.json")).unwrap_or(Value::Null);
        let task = launch.tasks.iter().find(|t| t.id == run.task_id);
        let variant = launch.variants.iter().find(|v| v.key == run.variant_key);
        Ok(json!({
            "run": run, "experiment": launch.experiment,
            "task": task, "variant": variant,
            "metrics": self.store.metrics_for_run(run_id)?,
            "grades": self.store.grades_for_run(run_id)?,
            "annotations": self.store.annotations_for_run(run_id)?,
            "transcript": transcript,
            "diff": read("diff.patch"),
            "stderr": crate::util::truncate_middle(&read("stderr.txt"), 20_000),
            "meta": meta,
        }))
    }
}

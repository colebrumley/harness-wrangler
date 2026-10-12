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

pub const TASK_TEMPLATE: &str = r#"id = "my-task"
prompt = "What the agent is asked to do. Say where to save the result if it is a file."
# fixture = { dir = "fixtures/my-task" }   # files the agent starts with
# output_file = "RESULT.md"                # what judges and reviewers read; default is the final message
expected_output = "One or two sentences describing a good result."
expectations = [
  "A specific, checkable statement about a good result",
]
tags = []

[graders]
deterministic = [
  # { kind = "word_count", path = "RESULT.md", max = 200 },
  # { kind = "command", cmd = "python3 -m unittest discover -s tests -q" },
]
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
            "guide" => self.guide()?,

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
                            out["estimate"] = self.estimate(&exp, tasks.len());
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
            "experiment_template" => match opt::<String>(args, "kind")?.as_deref() {
                // A trigger experiment: one overlay level, two turns, no judge.
                Some("trigger") => {
                    let skill: String = opt(args, "skill")?.unwrap_or_else(|| "my-skill".into());
                    let overlay: String = opt(args, "overlay")?.unwrap_or_else(|| format!("overlays/{skill}"));
                    json!({"toml": crate::trigger::experiment_template(&skill, &overlay, &format!("tasks/trigger/{}", crate::util::slug(&skill)))})
                }
                _ => json!({"toml": EXPERIMENT_TEMPLATE}),
            },
            "trigger_set" => json!(crate::trigger::generate(p, &arg::<String>(args, "skill")?, opt::<usize>(args, "n")?.unwrap_or(10).clamp(1, 100), &opt::<String>(args, "model")?.unwrap_or_else(|| "claude-sonnet-5-5".into()), &Arc::new(AtomicBool::new(false)))?),
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
            "overlays_list" => {
                let mut all = p.list_overlays();
                all.extend(p.list_skill_dirs());
                json!(all)
            }
            "task_template" => json!({"toml": TASK_TEMPLATE}),
            "task_delete" => {
                std::fs::remove_file(self.task_path(&arg::<String>(args, "source")?)?)?;
                json!({"ok": true})
            }

            // --------------------------------------------------- launches
            "launch_start" => {
                let name: String = arg(args, "experiment")?;
                let exp = p.load_experiment(&name)?;
                let (id, warnings) = scheduler::create_launch(p, s, &exp, None, &opt::<String>(args, "note")?.unwrap_or_default())?;
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: opt(args, "skip_judge")?.unwrap_or(false), retry_failed: false, budget_usd: None, baseline_only: opt(args, "baseline_only")?.unwrap_or(false) })?;
                json!({"launch_id": id, "warnings": warnings})
            }
            "launch_resume" => {
                let id: i64 = arg(args, "launch_id")?;
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: opt(args, "skip_judge")?.unwrap_or(false), retry_failed: opt(args, "retry_failed")?.unwrap_or(true), budget_usd: opt(args, "budget_usd")?, baseline_only: false })?;
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
                self.spawn_launch(id, RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: false, retry_failed: false, budget_usd: None, baseline_only: false })?;
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
                let opts = RunOptions { concurrency: opt(args, "concurrency")?, skip_judge: false, retry_failed: false, budget_usd: None, baseline_only: false };
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

    /// Rough time and cost for one launch, from this project's own history
    /// when it has any (same runner), else from typical figures.
    fn estimate(&self, exp: &crate::model::Experiment, n_tasks: usize) -> Value {
        let variants = exp.variants();
        let runs = n_tasks * variants.len() * exp.runs_per_cell as usize;
        let (mut wall, mut cost, mut n) = (0.0, 0.0, 0usize);
        for l in self.store.list_launches(None).unwrap_or_default().iter().take(8) {
            let Ok(launch) = self.store.launch(l.id) else { continue };
            if launch.def.runner.kind != exp.runner.kind { continue }
            for m in self.store.metrics_for_launch(l.id).unwrap_or_default().values() {
                if let Some(w) = m.get("wall_time_s") {
                    wall += w;
                    cost += m.get("cost_usd").copied().unwrap_or(0.0);
                    n += 1;
                }
            }
        }
        let mock = exp.runner.kind == "mock";
        let (avg_wall, avg_cost, basis) = if n >= 3 { (wall / n as f64, cost / n as f64, format!("from {n} earlier runs in this project")) } else if mock { (0.1, 0.0, "mock runner".to_string()) } else { (25.0, 0.08, "a typical short task; the first launch will replace this guess".to_string()) };
        let conc = exp.runner.concurrency.max(1) as f64;
        let judge = exp.judge();
        let nv = variants.len();
        let judge_calls = judge.as_ref().map(|c| {
            let per = c.pairwise_reps.unwrap_or(exp.runs_per_cell.min(3)) as usize;
            let all = c.pairwise_mode.clone().unwrap_or_else(|| if nv <= 4 { "all".into() } else { "baseline".into() }) == "all";
            let vp = if all { nv * nv.saturating_sub(1) / 2 } else { nv.saturating_sub(1) };
            (if c.rubric_scoring { runs } else { 0 }) + (if c.pairwise { vp * n_tasks * per * 2 } else { 0 })
        }).unwrap_or(0);
        let judge_mock = judge.as_ref().map(|c| c.model == "mock").unwrap_or(true);
        let secs = runs as f64 * avg_wall / conc + if judge_mock { 0.0 } else { judge_calls as f64 * 20.0 / conc };
        let judge_cost = if judge_mock { 0.0 } else { judge_calls as f64 * 0.06 };
        json!({"runs": runs, "judge_calls": judge_calls, "minutes": secs / 60.0, "agent_cost_usd": runs as f64 * avg_cost, "judge_cost_usd": judge_cost, "basis": basis})
    }

    /// Where this project stands and what to do next: the steps from an
    /// empty folder to acting on results, each marked done or not.
    fn guide(&self) -> Result<Value> {
        let (p, s) = (&self.project, &self.store);
        let tasks = p.list_all_tasks().into_iter().filter(|t| t.is_ok()).count();
        let mut patterns = p.list_overlays().into_iter().filter(|o| !crate::model::is_empty_overlay(o) && !o.contains("/ablation/")).collect::<Vec<_>>();
        patterns.extend(p.list_skill_dirs());
        let experiments = p.list_experiments();
        let launches = s.list_launches(None)?;
        let running = launches.iter().find(|l| l.status == "running");
        let finished = launches.iter().find(|l| l.status == "done" && l.runs_done > 0);
        let stopped = launches.iter().find(|l| matches!(l.status.as_str(), "interrupted" | "cancelled" | "budget" | "baseline") && l.runs_done < l.runs_total);
        let review = match finished {
            Some(l) if s.launch(l.id).map(|x| x.def.human_enabled()).unwrap_or(false) => Some((l.id, review::progress(s, l.id)?)),
            _ => None,
        };
        let first_exp = experiments.iter().find(|e| e.error.is_none()).map(|e| e.name.clone());
        let mut steps = vec![
            json!({"id": "tasks", "title": "Write a few tasks", "done": tasks > 0,
                "detail": if tasks > 0 { format!("{tasks} task file{} in tasks/.", if tasks == 1 { "" } else { "s" }) } else { "A task is a prompt plus the files the agent starts with and how the result is graded. Three to six tasks that differ from each other are a good start.".into() },
                "time": "5 to 10 minutes each, or ask the agent in the terminal to draft them", "action": {"label": if tasks > 0 { "Open Tasks" } else { "Add a task" }, "href": "/experiments/~tasks"}}),
            json!({"id": "pattern", "title": "Have something to test", "done": !patterns.is_empty(),
                "detail": if patterns.is_empty() { "A skill folder (with SKILL.md) or a folder holding a CLAUDE.md, anywhere in this project. Models and effort levels need nothing here.".to_string() } else { format!("Found {}: {}", patterns.len(), patterns.iter().take(4).cloned().collect::<Vec<_>>().join(", ")) },
                "time": "nothing to do if the skill is already in this repo", "action": Value::Null}),
            json!({"id": "experiment", "title": "Define an experiment", "done": !experiments.is_empty(),
                "detail": if experiments.is_empty() { "Choose what to vary (for example a skill against no skill, across three models) and which tasks to run.".to_string() } else { format!("{} experiment{}: {}", experiments.len(), if experiments.len() == 1 { "" } else { "s" }, experiments.iter().take(4).map(|e| e.name.clone()).collect::<Vec<_>>().join(", ")) },
                "time": "about 2 minutes in the form", "action": {"label": if experiments.is_empty() { "New experiment" } else { "Open experiments" }, "href": if experiments.is_empty() { "/experiments/new".to_string() } else { format!("/experiments/{}", first_exp.clone().unwrap_or_default()) }}}),
        ];
        // Optional, between defining and launching: run the baseline alone and
        // see whether the tasks leave any room for a pattern to show.
        let checked = launches.iter().find(|l| l.status == "baseline" || (l.status == "done" && l.runs_done > 0));
        steps.push(json!({"id": "headroom", "title": "Check the tasks leave room", "done": checked.is_some(), "optional": true,
            "detail": match launches.iter().find(|l| l.status == "baseline") {
                Some(l) => format!("Launch #{} of {} ran its baseline only. Its results show each task's headroom; resume it to run the variants.", l.id, l.experiment),
                None => "Run the baseline alone first. A task every run already passes cannot show a benefit, and you find out before paying for the variants. For one new task: wrangler task check <task>.".to_string(),
            },
            "time": "one variant's worth of runs",
            "action": checked.filter(|l| l.status == "baseline").map(|l| json!({"label": "Open the baseline", "href": format!("/results/{}", l.id)})).or_else(|| first_exp.clone().map(|n| json!({"label": "Open and run the baseline", "href": format!("/experiments/{n}")})))}));
        let run_detail = match (running, finished, stopped) {
            (Some(l), _, _) => format!("Launch #{} of {} is running: {} of {} runs done{}.", l.id, l.experiment, l.runs_done, l.runs_total, if l.phase.is_empty() || l.phase == "runs" { String::new() } else { format!(", now {}", l.phase) }),
            (None, Some(l), _) => format!("Launch #{} of {} finished: {} runs, ${:.2}.", l.id, l.experiment, l.runs_done, l.cost_usd),
            (None, None, Some(l)) if l.status == "baseline" => format!("Launch #{} of {} has its baseline: {} of {} runs. Open it to run the variants.", l.id, l.experiment, l.runs_done, l.runs_total),
            (None, None, Some(l)) => format!("Launch #{} of {} stopped at {} of {} runs. Open it to resume.", l.id, l.experiment, l.runs_done, l.runs_total),
            _ => "Every task runs under every variant several times, then the judge scores the results. You can leave it running.".to_string(),
        };
        let run_target = running.or(finished).or(stopped).map(|l| l.experiment.clone()).or(first_exp.clone());
        steps.push(json!({"id": "run", "title": "Run it", "done": finished.is_some(), "active": running.is_some(), "detail": run_detail,
            "time": "the experiment page shows an estimate before you start; typically minutes to an hour",
            "action": run_target.map(|n| json!({"label": if running.is_some() { "Watch progress" } else if finished.is_some() { "Open the launch" } else { "Open and run" }, "href": format!("/experiments/{n}")}))}));
        steps.push(match &review {
            Some((id, pr)) => json!({"id": "review", "title": "Review a few pairs yourself", "done": pr.recommended_left == 0 && pr.votes > 0 || pr.recommended == 0,
                "detail": if pr.recommended == 0 { "Nothing to compare in this launch.".to_string() } else if pr.recommended_left == 0 { format!("{} reviewed, one pair or more for every task. More is optional.", pr.votes) } else { format!("{} of {} done. Blind, side by side, keyboard driven. One pair per task, the one that settles the most.", pr.recommended - pr.recommended_left, pr.recommended) },
                "time": "about a minute per pair", "action": {"label": "Open Review", "href": format!("/review/{id}")}}),
            None => json!({"id": "review", "title": "Review a few pairs yourself", "done": false, "optional": true,
                "detail": "After a run, the app picks one pair per task where your judgment matters most. Optional, and worth it when the judge and the numbers disagree.", "time": "about a minute per pair", "action": Value::Null}),
        });
        steps.push(json!({"id": "results", "title": "Read the results and act on the advice", "done": false,
            "detail": match finished { Some(l) => format!("Results and advice for launch #{} are ready.", l.id), None => "Effect sizes with confidence intervals, per task and per model, then a short list of what to change.".to_string() },
            "time": "5 minutes", "action": finished.map(|l| json!({"label": "Open Results", "href": format!("/results/{}", l.id), "second": {"label": "Open Advice", "href": format!("/advice/{}", l.id)}}))}));
        let next = steps.iter().position(|st| st["done"] != true && st["optional"] != true).unwrap_or(steps.len() - 1);
        Ok(json!({"steps": steps, "next": next, "project": p.root.file_name().map(|n| n.to_string_lossy().to_string())}))
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
        let all_grades = self.store.grades_for_launch(id)?;
        let judge_grades: Vec<_> = all_grades.iter().filter(|g| g.tier == "judge").collect();
        let judged = judge_grades.len();
        let judgments = self.store.judgments(id, Some("judge"))?;
        let cfg = launch.def.judge();
        let label = |run_id: i64| runs.iter().find(|r| r.id == run_id).map(|r| (r.task_id.clone(), launch.variants.iter().find(|v| v.key == r.variant_key).map(|v| v.label.clone()).unwrap_or_default()));
        // What the judge has been saying, newest first.
        let mut feed: Vec<Value> = vec![];
        for g in judge_grades.iter().rev().take(8) {
            let (task, variant) = label(g.run_id).unwrap_or_default();
            let exps = g.detail["expectations"].as_array().cloned().unwrap_or_default();
            feed.push(json!({"kind": "score", "run_id": g.run_id, "task": task, "variant": variant, "score": g.score,
                "met": exps.iter().filter(|e| e["passed"] == true).count(), "of": exps.len(), "summary": g.detail["summary"]}));
        }
        let mut pair_feed: Vec<Value> = vec![];
        for j in judgments.iter().rev().take(8) {
            let (task, a) = label(j.run_a).unwrap_or_default();
            let (_, b) = label(j.run_b).unwrap_or_default();
            let winner = match j.winner.as_str() { "a" => a.clone(), "b" => b.clone(), _ => "tie".into() };
            pair_feed.push(json!({"kind": "pair", "task": task, "a": a, "b": b, "run_a": j.run_a, "run_b": j.run_b, "winner": winner, "consistent": j.consistent, "reason": j.reason}));
        }
        let to_score = runs.iter().filter(|r| r.is_complete() && launch.tasks.iter().find(|t| t.id == r.task_id).map(|t| t.graders.judge != Some(false)).unwrap_or(false)).count();
        let pairs_planned = cfg.as_ref().filter(|c| c.pairwise).map(|c| scheduler::judge_pairs(&launch, &runs, c).len()).unwrap_or(0);
        // Time left: remaining runs at the observed pace, then the judge calls still to make.
        let walls: Vec<f64> = runs.iter().filter_map(|r| metrics.get(&r.id)?.get("wall_time_s").copied()).collect();
        let avg_run = if walls.is_empty() { 30.0 } else { walls.iter().sum::<f64>() / walls.len() as f64 };
        let conc = launch.def.runner.concurrency.max(1) as f64;
        let runs_left = runs.iter().filter(|r| r.status == "pending" || r.status == "running").count() as f64;
        let total_pairs_expected = cfg.as_ref().filter(|c| c.pairwise).map(|c| {
            // Planned once every run is in; before that, scale up from the cells finished so far.
            let per = c.pairwise_reps.unwrap_or(launch.def.runs_per_cell.min(3)) as usize;
            let nv = launch.variants.len();
            let mode_all = c.pairwise_mode.clone().unwrap_or_else(|| if nv <= 4 { "all".into() } else { "baseline".into() }) == "all";
            let variant_pairs = if mode_all { nv * nv.saturating_sub(1) / 2 } else { nv.saturating_sub(1) };
            variant_pairs * launch.tasks.len() * per
        }).unwrap_or(0);
        let score_left = if cfg.as_ref().map(|c| c.rubric_scoring).unwrap_or(false) { (runs.len().saturating_sub(judged)) as f64 } else { 0.0 };
        let pair_left = total_pairs_expected.saturating_sub(judgments.len()) as f64;
        let judge_secs = 20.0;
        let eta_s = if launch.status == "running" { Some(runs_left * avg_run / conc + (score_left * judge_secs + pair_left * 2.0 * judge_secs) / conc) } else { None };
        Ok(json!({
            "launch_id": id, "experiment": launch.experiment, "status": launch.status, "phase": launch.phase,
            "created_at": launch.created_at, "finished_at": launch.finished_at, "note": launch.note, "suite": launch.suite,
            "active": self.jobs.lock().unwrap().contains_key(&id),
            "counts": {"total": runs.len(), "done": count("done"), "failed": count("failed"), "timeout": count("timeout"), "running": count("running"), "pending": count("pending"), "cancelled": count("cancelled")},
            "cost_usd": sum("cost_usd"), "judge_cost_usd": sum("judge_cost_usd"),
            "judged": judged, "judgments": judgments.len(),
            "judge": {"model": cfg.as_ref().map(|c| c.model.clone()), "rubric": cfg.as_ref().and_then(|c| c.rubric.clone()),
                "to_score": to_score, "scored": judged, "pairs_planned": pairs_planned.max(judgments.len()), "pairs_expected": total_pairs_expected, "pairs_done": judgments.len(),
                "scores": feed, "pairs": pair_feed},
            "eta_s": eta_s, "avg_run_s": avg_run,
            "tasks": launch.tasks.iter().map(|t| &t.id).collect::<Vec<_>>(),
            "variants": launch.variants.iter().map(|v| json!({"key": v.key, "label": v.label})).collect::<Vec<_>>(),
            "runs_per_cell": launch.def.runs_per_cell,
            "budget_usd": launch.def.budget_usd, "has_judge": launch.def.judge().is_some(), "human_review": launch.def.human_enabled(),
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

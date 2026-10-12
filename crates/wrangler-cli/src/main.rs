//! `wrangler`: the scriptable front end to wrangler-core. Every command
//! prints text for people and, with `--json`, JSON for agents and CI.

mod serve;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wrangler_core::ablation::{self, AblationRequest};
use wrangler_core::project::Suite;
use wrangler_core::scheduler::{self, Event, RunOptions};
use wrangler_core::{advice, interop, report, review, runner, stats, suite, Project, Store};

#[derive(Parser)]
#[command(name = "wrangler", version, about = "Controlled experiments over coding-agent harness patterns")]
struct Cli {
    /// Project directory (default: nearest parent with wrangler.toml or experiments/).
    #[arg(long, short = 'C', global = true)]
    project: Option<PathBuf>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a project layout in a directory.
    Init { dir: Option<PathBuf> },
    /// Check that the runners are installed and logged in.
    Doctor,
    /// List experiments, tasks, overlays, launches or suites.
    List { what: String },
    /// Check an experiment and show its cells without running anything.
    Validate { experiment: String },
    /// Run an experiment: every task × variant, N times, then grade and judge.
    Run {
        experiment: String,
        /// Override runs_per_cell.
        #[arg(long)]
        reps: Option<u32>,
        /// Override [runner] concurrency.
        #[arg(long)]
        concurrency: Option<usize>,
        /// Skip the LLM judge phase (run it later with `wrangler judge`).
        #[arg(long)]
        no_judge: bool,
        #[arg(long, default_value = "")]
        note: String,
        /// Spending cap in USD for this launch (overrides budget_usd).
        #[arg(long)]
        budget: Option<f64>,
        /// Run only the baseline variant, print each task's headroom, and
        /// leave the launch for `wrangler resume` to finish.
        #[arg(long)]
        baseline_only: bool,
    },
    /// Continue an interrupted or cancelled launch, or finish one left at its baseline.
    Resume {
        launch: String,
        /// Also rerun runs that failed or timed out.
        #[arg(long)]
        retry_failed: bool,
        #[arg(long)]
        concurrency: Option<usize>,
        #[arg(long)]
        no_judge: bool,
        /// New spending cap in USD for this launch (after a budget stop).
        #[arg(long)]
        budget: Option<f64>,
    },
    /// Run (or finish) the LLM judge phase of a launch.
    Judge {
        launch: String,
        #[arg(long)]
        concurrency: Option<usize>,
    },
    /// Per-cell progress and cost of a launch.
    Status { launch: String },
    /// Results: variants, factor effects with CIs, per-task breakdown, pairwise, ablation.
    Report {
        /// Launch id, or experiment name for its latest launch.
        launch: String,
        /// Primary metric for verdicts and the per-task table.
        #[arg(long)]
        metric: Option<String>,
    },
    /// List the runs of a launch.
    Runs {
        launch: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        variant: Option<String>,
    },
    /// Show one run: metrics and grades, or its output, diff or transcript.
    Show {
        run: i64,
        #[arg(long)]
        transcript: bool,
        #[arg(long)]
        diff: bool,
        #[arg(long)]
        output: bool,
    },
    /// Generate leave-one-section-out overlays and an experiment for a skill or CLAUDE.md.
    Ablate {
        /// SKILL.md, skill directory, CLAUDE.md, or an overlay holding one of them.
        source: String,
        #[arg(long)]
        name: Option<String>,
        /// Heading depth to split at.
        #[arg(long, default_value_t = 2)]
        depth: u32,
        /// Add a frontmatter-only variant to separate triggering from content.
        #[arg(long)]
        description_only: bool,
        /// Task globs (repeatable). Default tasks/**/*.toml.
        #[arg(long = "tasks")]
        tasks: Vec<String>,
        #[arg(long)]
        reps: Option<u32>,
        #[arg(long)]
        runner: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        judge_model: Option<String>,
        #[arg(long)]
        rubric: Option<String>,
        #[arg(long)]
        metric: Option<String>,
    },
    /// Tracked regression suites and per-model baselines.
    Suite {
        #[command(subcommand)]
        cmd: SuiteCmd,
    },
    /// Concrete suggestions with evidence, from a launch's results.
    Advice {
        launch: String,
        #[arg(long)]
        metric: Option<String>,
    },
    /// Human review annotations grouped by variant, for agents to read.
    Annotations { launch: String },
    /// Import tasks from another format.
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    /// Export results to another format.
    Export {
        #[command(subcommand)]
        cmd: ExportCmd,
    },
    /// Delete a launch and its run artifacts.
    Delete { launch: i64 },
    /// Serve the JSON API and the web UI on localhost.
    Serve {
        #[arg(long, default_value_t = 4477)]
        port: u16,
        /// Directory holding the built UI (default: apps/desktop/dist next to the repo, or $WRANGLER_UI_DIR).
        #[arg(long)]
        ui: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SuiteCmd {
    /// Create or replace a suite.
    Create {
        name: String,
        /// Experiment names (comma separated or repeated).
        #[arg(long, value_delimiter = ',', required = true)]
        experiments: Vec<String>,
        #[arg(long, default_value = "judge_score")]
        metric: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    List,
    /// Run the whole suite on a model and save the result as that model's baseline.
    Run {
        name: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        concurrency: Option<usize>,
    },
    /// The regression dashboard: each pattern's effect per model.
    Status { name: String },
    /// New-model workflow: what regressed, what is redundant, what still earns its keep.
    Diff {
        name: String,
        #[arg(long)]
        model: String,
        /// Model to compare against (default: the one baselined before it).
        #[arg(long)]
        previous: Option<String>,
    },
}

#[derive(Subcommand)]
enum ImportCmd {
    /// Import a skill-creator `evals/evals.json` as tasks and fixtures.
    SkillCreator {
        evals_json: PathBuf,
        /// Directory the evals' `files` paths are relative to (default: parent of evals/).
        #[arg(long)]
        skill_root: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ExportCmd {
    /// Export a launch as skill-creator `benchmark.json`.
    Benchmark {
        launch: String,
        /// Variant to export as with_skill (label or key; default: second variant).
        #[arg(long)]
        with: Option<String>,
        /// Variant to export as without_skill (default: first variant).
        #[arg(long)]
        without: Option<String>,
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    // Second Ctrl-C exits immediately.
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(130) };
    }
}

/// A cancel flag that flips on Ctrl-C, so runs stop cleanly and stay resumable.
fn cancel_on_sigint() -> Arc<AtomicBool> {
    unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as usize) };
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            eprintln!("\ninterrupted: stopping runs (resume with `wrangler resume`). Ctrl-C again to exit now.");
            c.store(true, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    });
    cancel
}

fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Print scheduler events as progress lines (text) or JSON lines.
fn progress_sink(json: bool) -> impl Fn(Event) + Sync {
    let cost = std::sync::Mutex::new(0.0f64);
    move |e: Event| {
        if json {
            eprintln!("{}", serde_json::to_string(&e).unwrap_or_default());
            return;
        }
        match e {
            Event::Started { launch_id, total, pending, concurrency } => eprintln!("launch #{launch_id}: {pending} of {total} runs to do, {concurrency} at a time"),
            Event::RunFinished { task, variant, rep, status, cost_usd, wall_s, done, total, error, run_id } => {
                let mut c = cost.lock().unwrap();
                *c += cost_usd.unwrap_or(0.0);
                let w = total.to_string().len();
                let price = cost_usd.map(|x| format!("${x:.4}")).unwrap_or_else(|| "      –".into());
                eprintln!("[{done:>w$}/{total}] {status:<8} {task} · {variant} · rep {} {price} {wall_s:>6.1}s  total ${:.4}  (run {run_id})", rep + 1, *c);
                if !error.is_empty() {
                    eprintln!("         {}", error.lines().next().unwrap_or(""));
                }
            }
            Event::RateLimited { run_id, wait_s, attempt, detail } => eprintln!("rate limited (run {run_id}, attempt {attempt}): waiting {wait_s}s. {}", detail.lines().next().unwrap_or("")),
            Event::Phase { name, total } => eprintln!("{name}: {total} item(s)"),
            Event::JudgeProgress { done, total } => {
                if done == total || done % 5 == 0 {
                    eprintln!("  judged {done}/{total}")
                }
            }
            Event::Warning { message } => eprintln!("warning: {message}"),
            Event::Finished { launch_id, status } => eprintln!("launch #{launch_id} {status}"),
            Event::RunStarted { .. } => {}
        }
    }
}

fn open(cli: &Cli) -> Result<(Project, Store)> {
    let project = match &cli.project {
        Some(p) => Project::open(p)?,
        None => Project::discover(&std::env::current_dir()?)?,
    };
    let store = Store::open(&project.db_path())?;
    store.mark_stale()?;
    Ok((project, store))
}

fn doctor(json: bool) -> Result<()> {
    let probe = |bin: &str, args: &[&str]| -> (bool, String) {
        match std::process::Command::new(bin).args(args).stdin(std::process::Stdio::null()).output() {
            Ok(o) if o.status.success() => (true, String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string()),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("failed").to_string()),
            Err(e) => (false, format!("not found ({e})")),
        }
    };
    let checks = vec![
        ("claude", probe("claude", &["--version"]), "Claude Code runner and LLM judges"),
        ("codex", probe("codex", &["--version"]), "Codex runner (optional)"),
        ("git", probe("git", &["--version"]), "workspace baselines and diffs"),
        ("sandbox-exec", (std::path::Path::new("/usr/bin/sandbox-exec").exists(), "/usr/bin/sandbox-exec".into()), "seatbelt isolation (macOS)"),
    ];
    if json {
        return print_json(&checks.iter().map(|(n, (ok, d), why)| json!({"tool": n, "ok": ok, "detail": d, "used_for": why})).collect::<Vec<_>>());
    }
    for (name, (ok, detail), why) in &checks {
        println!("{} {name:<13} {detail}  — {why}", if *ok { "ok  " } else { "MISSING" });
    }
    println!("\nRunners: {}. Login is checked at run time: a logged-out runner fails its first run with the CLI's own message.", runner::known().join(", "));
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = real_main(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main(cli: Cli) -> Result<()> {
    let json = cli.json;
    match &cli.cmd {
        Cmd::Init { dir } => {
            let dir = dir.clone().or(cli.project.clone()).unwrap_or(std::env::current_dir()?);
            let p = Project::init(&dir)?;
            if json {
                return print_json(&json!({"root": p.root}));
            }
            println!("initialized wrangler project at {}", p.root.display());
            println!("next: add tasks under tasks/, overlays under overlays/, and an experiment under experiments/ (see docs/formats.md)");
            return Ok(());
        }
        Cmd::Doctor => return doctor(json),
        _ => {}
    }
    let (project, store) = open(&cli)?;
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Doctor => unreachable!(),
        Cmd::List { what } => match what.as_str() {
            "experiments" | "experiment" => {
                let list = project.list_experiments();
                if json {
                    return print_json(&list);
                }
                for e in list {
                    match e.error {
                        Some(err) => println!("{:<32} INVALID: {err}", e.name),
                        None => println!("{:<32} {} variants × {} tasks × {} reps{}  {}", e.name, e.variants, e.tasks, e.runs_per_cell, if e.ablation { "  [ablation]" } else { "" }, e.description),
                    }
                }
            }
            "tasks" | "task" => {
                let mut rows = vec![];
                for t in project.list_all_tasks() {
                    match t {
                        Ok(t) => rows.push(json!({"id": t.id, "source": t.source, "tags": t.tags, "expectations": t.expectations.len(), "deterministic_graders": t.graders.deterministic.len()})),
                        Err(e) => rows.push(json!({"error": format!("{e:#}")})),
                    }
                }
                if json {
                    return print_json(&rows);
                }
                for r in rows {
                    match r.get("error") {
                        Some(e) => println!("INVALID: {}", e.as_str().unwrap_or("")),
                        None => println!("{:<28} {:<44} tags: {}", r["id"].as_str().unwrap_or(""), r["source"].as_str().unwrap_or(""), r["tags"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()),
                    }
                }
            }
            "overlays" | "overlay" => {
                let list = project.list_overlays();
                if json {
                    return print_json(&list);
                }
                list.iter().for_each(|o| println!("{o}"));
            }
            "launches" | "launch" => {
                let list = store.list_launches(None)?;
                if json {
                    return print_json(&list);
                }
                for l in list {
                    println!("#{:<5} {:<30} {:<12} {}/{} done, {} failed  ${:.4}  {}  {}", l.id, l.experiment, l.status, l.runs_done, l.runs_total, l.runs_failed, l.cost_usd, l.created_at, l.note);
                }
            }
            "suites" | "suite" => {
                let list = project.list_suites();
                if json {
                    return print_json(&list);
                }
                for s in list {
                    println!("{:<24} {} · {}", s.name, s.primary_metric, s.experiments.join(", "));
                }
            }
            other => bail!("cannot list `{other}` (try experiments, tasks, overlays, launches, suites)"),
        },
        Cmd::Validate { experiment } => {
            let exp = project.load_experiment(&experiment)?;
            let warnings = project.validate(&exp)?;
            let tasks = project.load_tasks(&exp)?;
            let variants = exp.variants();
            let total = tasks.len() * variants.len() * exp.runs_per_cell as usize;
            if json {
                return print_json(&json!({"ok": true, "experiment": exp, "variants": variants, "tasks": tasks.iter().map(|t| &t.id).collect::<Vec<_>>(), "total_runs": total, "warnings": warnings}));
            }
            println!("{}: ok", exp.name);
            println!("{} variants × {} tasks × {} reps = {total} runs · isolation {:?} · concurrency {}", variants.len(), tasks.len(), exp.runs_per_cell, exp.isolation, exp.runner.concurrency);
            for v in &variants {
                println!("  variant  {}  [runner {} · model {} · effort {}]", v.label, v.runner, v.model.as_deref().unwrap_or("default"), v.effort.as_deref().unwrap_or("default"));
            }
            for t in &tasks {
                println!("  task     {}  ({})", t.id, t.source);
            }
            for w in warnings {
                println!("warning: {w}");
            }
        }
        Cmd::Run { experiment, reps, concurrency, no_judge, note, budget, baseline_only } => {
            let mut exp = project.load_experiment(&experiment)?;
            if budget.is_some() {
                exp.budget_usd = budget;
            }
            if let Some(r) = reps {
                exp.runs_per_cell = r.max(1);
            }
            let (id, warnings) = scheduler::create_launch(&project, &store, &exp, None, &note)?;
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            let cancel = cancel_on_sigint();
            scheduler::execute(&project, &store, id, &RunOptions { concurrency, skip_judge: no_judge, retry_failed: false, budget_usd: None, baseline_only }, cancel, &progress_sink(json))?;
            if baseline_only && !json {
                let rep = report::build(&store, id, None)?;
                print!("{}", report::render_headroom_text(&rep));
                if rep.status == "baseline" {
                    eprintln!("\nbaseline done; the other variants have not run. Finish with: wrangler resume {id}");
                }
                return Ok(());
            }
            finish_run_output(&store, id, json)?;
        }
        Cmd::Resume { launch, retry_failed, concurrency, no_judge, budget } => {
            let id = store.resolve_launch(&launch)?;
            let cancel = cancel_on_sigint();
            scheduler::execute(&project, &store, id, &RunOptions { concurrency, skip_judge: no_judge, retry_failed, budget_usd: budget, baseline_only: false }, cancel, &progress_sink(json))?;
            finish_run_output(&store, id, json)?;
        }
        Cmd::Judge { launch, concurrency } => {
            let id = store.resolve_launch(&launch)?;
            let l = store.launch(id)?;
            if l.def.judge().is_none() {
                bail!("experiment `{}` has no [graders.default] judge", l.experiment);
            }
            let cancel = cancel_on_sigint();
            store.set_launch_status(id, "running", "judging")?;
            let res = scheduler::judge_launch(&project, &store, id, concurrency.unwrap_or(l.def.runner.concurrency), &cancel, &progress_sink(json));
            store.set_launch_status(id, if cancel.load(Ordering::Relaxed) { "cancelled" } else { "done" }, "")?;
            res?;
            finish_run_output(&store, id, json)?;
        }
        Cmd::Status { launch } => {
            let id = store.resolve_launch(&launch)?;
            let rep = report::build(&store, id, None)?;
            if json {
                let cells: Vec<Value> = rep.tasks.iter().flat_map(|t| t.cells.iter().map(move |c| json!({"task": t.task_id, "variant": c.variant_key, "counts": c.counts, "cost_usd": c.metrics.get("cost_usd").map(|m| m.mean * m.n as f64)}))).collect();
                return print_json(&json!({"launch_id": id, "experiment": rep.experiment, "status": rep.status, "phase": rep.phase, "counts": rep.counts, "cost_usd": rep.cost_usd, "judge_cost_usd": rep.judge_cost_usd, "cells": cells}));
            }
            println!("{} launch #{id}: {}{}", rep.experiment, rep.status, if rep.phase.is_empty() { String::new() } else { format!(" ({})", rep.phase) });
            println!("{}/{} done · {} running · {} pending · {} failed · {} timeout · cost ${:.4} + judge ${:.4}", rep.counts.done, rep.counts.total, rep.counts.running, rep.counts.pending, rep.counts.failed, rep.counts.timeout, rep.cost_usd, rep.judge_cost_usd);
            if rep.status == "baseline" {
                println!("baseline phase done: only the baseline variant has run. `wrangler resume {id}` runs the other {} variant(s).", rep.variants.len().saturating_sub(1));
                print!("{}", report::render_headroom_text(&rep));
            }
            println!("\ncells: done/total per task × variant (cost)");
            for t in &rep.tasks {
                println!("  {}", t.task_id);
                for c in &t.cells {
                    let label = rep.variants.iter().find(|v| v.key == c.variant_key).map(|v| v.label.as_str()).unwrap_or("");
                    let cost = c.metrics.get("cost_usd").map(|m| format!("${:.4}", m.mean * m.n as f64)).unwrap_or_else(|| "–".into());
                    let bad = c.counts.failed + c.counts.timeout;
                    println!("    {:<44} {}/{}{}  {cost}", label, c.counts.done, c.counts.total, if bad > 0 { format!(" ({bad} failed)") } else { String::new() });
                }
            }
        }
        Cmd::Report { launch, metric } => {
            let id = store.resolve_launch(&launch)?;
            let rep = report::build(&store, id, metric.as_deref())?;
            if json {
                return print_json(&rep);
            }
            print!("{}", report::render_text(&rep));
        }
        Cmd::Runs { launch, status, task, variant } => {
            let id = store.resolve_launch(&launch)?;
            let l = store.launch(id)?;
            let metrics = store.metrics_for_launch(id)?;
            let label = |key: &str| l.variants.iter().find(|v| v.key == key).map(|v| v.label.clone()).unwrap_or_default();
            let runs: Vec<_> = store
                .runs(id)?
                .into_iter()
                .filter(|r| status.as_ref().map(|s| &r.status == s).unwrap_or(true))
                .filter(|r| task.as_ref().map(|t| &r.task_id == t).unwrap_or(true))
                .filter(|r| variant.as_ref().map(|v| &r.variant_key == v || &label(&r.variant_key) == v).unwrap_or(true))
                .collect();
            if json {
                return print_json(&runs.iter().map(|r| json!({"id": r.id, "task": r.task_id, "variant": label(&r.variant_key), "variant_key": r.variant_key, "rep": r.rep, "status": r.status, "error": r.error, "dir": r.dir, "metrics": metrics.get(&r.id)})).collect::<Vec<_>>());
            }
            for r in runs {
                let m = metrics.get(&r.id);
                let g = |k: &str, f: &dyn Fn(f64) -> String| m.and_then(|m| m.get(k)).map(|v| f(*v)).unwrap_or_else(|| "–".into());
                println!(
                    "{:<6} {:<8} {:<22} {:<36} rep {}  {}  {}  judge {}  pass {}  {}",
                    r.id,
                    r.status,
                    r.task_id,
                    label(&r.variant_key),
                    r.rep + 1,
                    g("cost_usd", &|v| format!("${v:.4}")),
                    g("wall_time_s", &|v| format!("{v:.1}s")),
                    g("judge_score", &|v| format!("{v:.0}")),
                    g("pass", &|v| format!("{v:.0}")),
                    r.error.lines().next().unwrap_or("")
                );
            }
        }
        Cmd::Show { run, transcript, diff, output } => {
            let r = store.run(run)?;
            let dir = std::path::Path::new(&r.dir);
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
            let tr: wrangler_core::transcript::Transcript = serde_json::from_str(&read("transcript.json")).unwrap_or_default();
            if json {
                return print_json(&json!({"run": r, "metrics": store.metrics_for_run(run)?, "grades": store.grades_for_run(run)?, "annotations": store.annotations_for_run(run)?, "transcript": tr, "diff": read("diff.patch")}));
            }
            if transcript {
                print!("{}", tr.render(4000));
            } else if diff {
                print!("{}", read("diff.patch"));
            } else if output {
                println!("{}", r.output_text);
            } else {
                println!("run {} · {} · {} · rep {} · {}", r.id, r.task_id, r.variant_key, r.rep + 1, r.status);
                if !r.error.is_empty() {
                    println!("error: {}", r.error);
                }
                println!("model: {} · skills invoked: {} · artifacts: {}", r.model, if r.skills.is_empty() { "none".into() } else { r.skills.join(", ") }, r.dir);
                for e in r.events.as_array().into_iter().flatten() {
                    println!("event: {} at {}ms: {}", e["kind"].as_str().unwrap_or(""), e["at_ms"], e["detail"].as_str().unwrap_or(""));
                }
                println!("\nmetrics");
                for (k, v) in store.metrics_for_run(run)? {
                    println!("  {k:<24} {}", report::fmt_value(&stats::metric_info(&k), v));
                }
                println!("\ngrades");
                for g in store.grades_for_run(run)? {
                    let verdict = match g.passed {
                        Some(true) => "pass",
                        Some(false) => "FAIL",
                        None => "–",
                    };
                    println!("  [{}] {:<28} {verdict}{}", g.tier, g.grader, g.score.map(|s| format!("  score {s}")).unwrap_or_default());
                    for e in g.detail["expectations"].as_array().into_iter().flatten() {
                        println!("      {} {} — {}", if e["passed"] == true { "✓" } else { "✗" }, e["text"].as_str().unwrap_or(""), e["evidence"].as_str().unwrap_or(""));
                    }
                }
                println!("\n(use --output, --diff or --transcript for the content)");
            }
        }
        Cmd::Ablate { source, name, depth, description_only, tasks, reps, runner, model, effort, judge_model, rubric, metric } => {
            let plan = ablation::generate(&project, &AblationRequest { source, name, depth: Some(depth), description_only, tasks, runs_per_cell: reps, runner, model, effort, judge_model, rubric, primary_metric: metric })?;
            if json {
                return print_json(&plan);
            }
            println!("wrote {} ({} variants)", plan.experiment_path, plan.overlays.len());
            for s in &plan.sections {
                println!("  section {:>2}  lines {:>4}–{:<4} ~{:>5} tokens  {}", s.index + 1, s.start_line, s.end_line, s.est_tokens, s.title);
            }
            for s in &plan.skipped_sections {
                println!("  skipped (heading only): {s}");
            }
            println!("next: wrangler run {}", plan.experiment);
        }
        Cmd::Suite { cmd } => match cmd {
            SuiteCmd::Create { name, experiments, metric, description } => {
                project.save_suite(&Suite { name: name.clone(), description, primary_metric: metric, experiments })?;
                if json {
                    return print_json(&json!({"ok": true, "suite": name}));
                }
                println!("wrote suites/{name}.toml\nnext: wrangler suite run {name} --model <model>");
            }
            SuiteCmd::List => {
                let list = project.list_suites();
                if json {
                    return print_json(&list);
                }
                for s in list {
                    let models: Vec<String> = store.baselines(&s.name)?.into_iter().map(|b| b.model).fold(vec![], |mut a, m| {
                        if !a.contains(&m) {
                            a.push(m)
                        }
                        a
                    });
                    println!("{:<24} {} · {} · baselines: {}", s.name, s.primary_metric, s.experiments.join(", "), if models.is_empty() { "none".into() } else { models.join(", ") });
                }
            }
            SuiteCmd::Run { name, model, concurrency } => {
                let cancel = cancel_on_sigint();
                let launches = suite::run_model(&project, &store, &name, &model, &RunOptions { concurrency, skip_judge: false, retry_failed: false, budget_usd: None, baseline_only: false }, cancel, &progress_sink(json))?;
                let dash = suite::dashboard(&project, &store, &name)?;
                if json {
                    return print_json(&json!({"launches": launches, "dashboard": dash}));
                }
                print!("\n{}", suite::render_dashboard_text(&dash));
            }
            SuiteCmd::Status { name } => {
                let dash = suite::dashboard(&project, &store, &name)?;
                if json {
                    return print_json(&dash);
                }
                print!("{}", suite::render_dashboard_text(&dash));
            }
            SuiteCmd::Diff { name, model, previous } => {
                let d = suite::diff(&project, &store, &name, &model, previous.as_deref())?;
                if json {
                    return print_json(&d);
                }
                print!("{}", suite::render_diff_text(&d));
            }
        },
        Cmd::Advice { launch, metric } => {
            let id = store.resolve_launch(&launch)?;
            let suggestions = advice::build(&store, id, metric.as_deref())?;
            let rep = report::build(&store, id, metric.as_deref())?;
            if json {
                return print_json(&json!({"launch_id": id, "experiment": rep.experiment, "primary_metric": rep.primary_metric, "suggestions": suggestions}));
            }
            print!("{}", advice::render_text(&rep, &suggestions));
        }
        Cmd::Annotations { launch } => {
            let id = store.resolve_launch(&launch)?;
            let digest = review::annotation_digest(&store, id)?;
            if json {
                return print_json(&digest);
            }
            for v in digest {
                if v.annotations.is_empty() {
                    continue;
                }
                println!("{} — {} annotated of {} reviewed runs", v.variant, v.annotated_runs, v.reviewed_runs);
                for (e, n) in &v.emoji {
                    println!("  {e} × {n}");
                }
                for p in &v.passages {
                    println!("  flagged in {} run(s): \"{}\"", p.runs, p.quote);
                    for c in &p.comments {
                        println!("      – {c}");
                    }
                }
            }
        }
        Cmd::Import { cmd: ImportCmd::SkillCreator { evals_json, skill_root } } => {
            let written = interop::import_skill_creator(&project, &evals_json, skill_root.as_deref())?;
            if json {
                return print_json(&written);
            }
            println!("imported {} task(s):", written.len());
            written.iter().for_each(|w| println!("  {w}"));
        }
        Cmd::Export { cmd: ExportCmd::Benchmark { launch, with, without, output } } => {
            let id = store.resolve_launch(&launch)?;
            let v = interop::export_benchmark(&store, id, with.as_deref(), without.as_deref())?;
            match output {
                Some(path) => {
                    std::fs::write(&path, serde_json::to_string_pretty(&v)?).with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("wrote {}", path.display());
                }
                None => print_json(&v)?,
            }
        }
        Cmd::Delete { launch } => {
            store.delete_launch(launch)?;
            let _ = std::fs::remove_dir_all(project.runs_dir().join(launch.to_string()));
            if json {
                return print_json(&json!({"ok": true}));
            }
            println!("deleted launch #{launch}");
        }
        Cmd::Serve { port, ui } => {
            drop(store);
            serve::serve(project, port, ui)?;
        }
    }
    Ok(())
}

fn finish_run_output(store: &Store, id: i64, json: bool) -> Result<()> {
    let rep = report::build(store, id, None)?;
    if json {
        return print_json(&rep);
    }
    print!("\n{}", report::render_text(&rep));
    eprintln!("\nmore: wrangler advice {id} · wrangler runs {id} · wrangler show <run>");
    Ok(())
}

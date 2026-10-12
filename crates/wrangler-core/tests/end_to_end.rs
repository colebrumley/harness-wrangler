mod common;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use wrangler_core::scheduler::{self, Event, RunOptions};
use wrangler_core::stats::Verdict;
use wrangler_core::{advice, report, stats, Store};

#[test]
fn mock_experiment_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, warnings) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let events = Mutex::new(vec![]);
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|e| events.lock().unwrap().push(e)).unwrap();

    let runs = store.runs(launch).unwrap();
    assert_eq!(runs.len(), 3 * 6 * 4);
    assert!(runs.iter().all(|r| r.status == "done"), "{:?}", runs.iter().find(|r| r.status != "done"));
    let events = events.into_inner().unwrap();
    assert!(matches!(events.last(), Some(Event::Finished { status, .. }) if status == "done"));
    assert!(events.iter().any(|e| matches!(e, Event::Phase { name, .. } if name == "pairwise")));

    // Artifacts on disk, workspace gone, overlay excluded from the diff.
    let r = runs.iter().find(|r| r.variant_key.starts_with("overlay=plain")).unwrap();
    let dir = std::path::Path::new(&r.dir);
    for f in ["transcript.json", "raw.jsonl", "diff.patch", "final.txt", "meta.json", "files/SUMMARY.md"] {
        assert!(dir.join(f).exists(), "missing {f}");
    }
    let patch = std::fs::read_to_string(dir.join("diff.patch")).unwrap();
    assert!(patch.contains("SUMMARY.md") && !patch.contains("SKILL.md"));
    assert_eq!(r.skills, vec!["plain"]);
    assert!(r.output_text.contains("QUALITY="));

    let rep = report::build(&store, launch, None).unwrap();
    assert_eq!(rep.primary_metric, "judge_score");
    assert_eq!(rep.counts.done, 72);
    assert_eq!(rep.variants.len(), 6);
    let find = |level: &str, metric: &str| rep.comparisons.iter().find(|c| c.kind == "factor" && c.level == level && c.metric == metric).unwrap();
    // The mock skill adds ~0.15 quality (≈ +1.4 judge points); the noisy CLAUDE.md removes 0.2.
    let plain = find("plain", "judge_score");
    assert_eq!(plain.effect.verdict, Verdict::Better, "{plain:?}");
    assert!(plain.effect.effect > 0.8 && plain.effect.effect < 2.2);
    assert_eq!(plain.effect.units, 6, "paired by task × effort");
    assert_eq!(find("noisy", "judge_score").effect.verdict, Verdict::Worse);
    assert_eq!(find("high", "cost_usd").effect.verdict, Verdict::Worse);
    assert!(find("plain", "pass").effect.effect > 0.0);
    // Variant-level comparisons exist because there are two factors.
    assert!(rep.comparisons.iter().any(|c| c.kind == "variant"));
    // skill_invoked only exists where an overlay carries a skill.
    let plain_v = rep.variants.iter().find(|v| v.key == "overlay=plain|effort=medium").unwrap();
    assert_eq!(plain_v.metrics["skill_invoked"].mean, 1.0);
    assert!(!rep.variants[0].metrics.contains_key("skill_invoked"));

    // Per-level effects: the skill's effect within each effort level, and
    // how the second level's effect differs from the first's.
    let cond: Vec<_> = rep.conditional.iter().filter(|c| c.factor == "overlay" && c.level == "plain" && c.by_factor == "effort" && c.metric == "judge_score").collect();
    assert_eq!(cond.iter().map(|c| c.by_level.as_str()).collect::<Vec<_>>(), vec!["medium", "high"]);
    assert!(cond.iter().all(|c| c.effect.effect > 0.0 && c.effect.units == 3 && c.effect.n_a == 12), "{cond:?}");
    assert_eq!(cond[0].effect.verdict, Verdict::Better);
    assert!(cond[0].vs_first.is_none());
    let d = cond[1].vs_first.as_ref().unwrap();
    // The mock skill helps equally at both effort levels, so there is no interaction.
    assert!(!d.differs && d.ci_low < 0.0 && d.ci_high > 0.0, "{d:?}");
    assert!((d.diff - (cond[1].effect.effect - cond[0].effect.effect)).abs() < 1e-9);
    assert!(report::render_text(&rep).contains("OVERLAY \"plain\" WITHIN EACH EFFORT"));

    let pw = rep.pairwise.as_ref().unwrap();
    assert_eq!(pw.rankings[0].source, "judge");
    // baseline mode: 5 other variants × 3 tasks × 3 reps
    assert_eq!(pw.position_bias.as_ref().unwrap().pairs, 45);
    let s = &pw.rankings[0].strengths;
    assert!(s[2].strength > s[4].strength, "plain should outrank noisy: {s:?}");

    let text = report::render_text(&rep);
    assert!(text.contains("FACTOR overlay") && text.contains("PAIRWISE") && text.contains("position bias"), "{text}");

    // Judging again adds nothing; the phase is idempotent.
    let before = store.judgments(launch, None).unwrap().len();
    scheduler::judge_launch(&project, &store, launch, 2, &Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    assert_eq!(store.judgments(launch, None).unwrap().len(), before);
}

#[test]
fn ablation_attributes_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let plan = wrangler_core::ablation::generate(
        &project,
        &wrangler_core::ablation::AblationRequest {
            source: "overlays/plain".into(),
            description_only: true,
            placebo: true,
            runner: Some("mock".into()),
            judge_model: Some("mock".into()),
            runs_per_cell: Some(6),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(plan.sections.len(), 4);
    let exp = project.load_experiment(&plan.experiment).unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let rep = report::build(&store, launch, None).unwrap();
    let ab = rep.ablation.unwrap();
    assert_eq!(ab.full_effect.as_ref().unwrap().verdict, Verdict::Better);
    let by_title = |t: &str| ab.sections.iter().find(|s| s.title == t).unwrap();
    assert_eq!(by_title("Lead with impact").attribution, "carries");
    assert_eq!(by_title("Add flourish").attribution, "hurts");
    assert!(matches!(by_title("Cut hedges").attribution.as_str(), "no_effect" | "inconclusive"));
    // The description alone triggers the skill but carries no content effect in the mock.
    assert!(ab.description_only_effect.is_some() && ab.body_effect.is_some());
    assert_eq!(ab.skill_invoked_rate, Some(1.0));
    assert!(report::render_ablation_text(&ab).contains("carries"));
    // The mock responds to the skill's own lines and not to the placebo
    // corpus, so the gain is from the content.
    let placebo = ab.placebo.as_ref().expect("placebo section");
    assert_eq!(placebo.attribution, "content", "{placebo:?}");
    assert_eq!(placebo.full_vs_placebo.as_ref().unwrap().verdict, Verdict::Better);
    assert!(placebo.placebo_vs_none.as_ref().unwrap().verdict != Verdict::Better);
    assert!(report::render_ablation_text(&ab).contains("full vs placebo"));
    assert!(advice::build(&store, launch, None).unwrap().iter().all(|s| s.kind != "placebo_explains"));

    // A CLAUDE.md that helps only as much as its own placebo: presence, and advice says so.
    common::write(tmp.path(), "overlays/vague/CLAUDE.md", "# Notes\n\nBe good at this.\n\n## More\n\nTry hard on every task you get.\n");
    let plan = wrangler_core::ablation::generate(&project, &wrangler_core::ablation::AblationRequest { source: "overlays/vague".into(), placebo: true, runner: Some("mock".into()), judge_model: Some("mock".into()), runs_per_cell: Some(30), ..Default::default() }).unwrap();
    // Give both the file and its placebo the same lift, as "any instructions help" would.
    for variant in ["full", "placebo"] {
        let f = tmp.path().join(format!("overlays/ablation/{}/{variant}/CLAUDE.md", plan.experiment));
        let text = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&f, format!("{text}\nmock-effect: 0.25\n")).unwrap();
    }
    let exp = project.load_experiment(&plan.experiment).unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let rep = report::build(&store, launch, None).unwrap();
    let p = rep.ablation.as_ref().unwrap().placebo.as_ref().unwrap();
    assert_eq!(p.attribution, "presence", "{p:#?}");
    let adv = advice::build(&store, launch, None).unwrap();
    let s = adv.iter().find(|s| s.kind == "placebo_explains").expect("placebo_explains");
    assert_eq!(s.priority, 1);
    assert!(s.action.starts_with("The gain comes from having instructions, not from this content; a shorter file would do."));
    assert!(s.evidence["full_vs_placebo"]["verdict"] == "neutral" && s.evidence["placebo_vs_none"]["verdict"] == "better");
}

#[test]
fn cancel_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let seen = std::sync::atomic::AtomicUsize::new(0);
    scheduler::execute(&project, &store, launch, &RunOptions { concurrency: Some(2), ..Default::default() }, cancel, &|e| {
        if matches!(e, Event::RunFinished { .. }) && seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 9 {
            c2.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .unwrap();
    assert_eq!(store.launch(launch).unwrap().status, "cancelled");
    let done = store.runs(launch).unwrap().iter().filter(|r| r.status == "done").count();
    assert!((10..72).contains(&done), "{done}");
    assert!(store.judgments(launch, None).unwrap().is_empty());

    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    assert_eq!(store.launch(launch).unwrap().status, "done");
    assert!(store.runs(launch).unwrap().iter().all(|r| r.status == "done"));
}

/// Baseline first: the first variant's runs lead the queue, `baseline_only`
/// stops after them with the launch resumable, and a task every run passes
/// yields "no headroom" instead of a verdict on the pattern.
#[test]
fn baseline_first_and_headroom() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    // Every mock run writes SUMMARY.md, so `pass` is saturated at 1 in every arm.
    common::write(
        tmp.path(),
        "experiments/saturated.toml",
        r#"name = "saturated"
runs_per_cell = 3
primary_metric = "pass"

[runner]
kind = "mock"
concurrency = 1

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none", "overlays/plain", "overlays/noisy"]

[tasks]
include = ["tasks/*.toml"]

[graders.default]
deterministic = [{ kind = "file_exists", path = "SUMMARY.md" }]
"#,
    );
    for id in ["alpha", "bravo", "charlie"] {
        common::write(tmp.path(), &format!("tasks/{id}.toml"), &format!("id = \"{id}\"\nprompt = \"Summarize REPORT.md into SUMMARY.md\"\nfixture = {{ dir = \"fixtures/incident\" }}\noutput_file = \"SUMMARY.md\"\n"));
    }
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("saturated").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    let order = Mutex::new(Vec::<String>::new());
    let sink = |e: Event| {
        if let Event::RunFinished { variant, .. } = e {
            order.lock().unwrap().push(variant);
        }
    };
    let no_cancel = || Arc::new(AtomicBool::new(false));

    scheduler::execute(&project, &store, launch, &RunOptions { baseline_only: true, ..Default::default() }, no_cancel(), &sink).unwrap();
    let baseline_runs = 3 * 3; // runs_per_cell × tasks
    assert_eq!(order.lock().unwrap().len(), baseline_runs);
    assert!(order.lock().unwrap().iter().all(|v| v == "none"));
    assert_eq!(store.launch(launch).unwrap().status, "baseline");
    let runs = store.runs(launch).unwrap();
    assert_eq!(runs.iter().filter(|r| r.status == "pending").count(), 2 * baseline_runs);
    assert!(runs.iter().filter(|r| r.status == "done").all(|r| r.variant_key == "overlay=none"));
    let rep = report::build(&store, launch, None).unwrap();
    assert!(rep.tasks.iter().all(|t| t.headroom.describe() == "none (ceiling)"), "{:?}", rep.tasks.iter().map(|t| t.headroom.describe()).collect::<Vec<_>>());
    assert!(report::render_headroom_text(&rep).contains("Every task is saturated"));

    // Resume finishes the rest, and a fresh launch still runs the baseline first.
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &sink).unwrap();
    assert_eq!(store.launch(launch).unwrap().status, "done");
    assert_eq!(order.lock().unwrap().len(), 3 * baseline_runs);
    order.lock().unwrap().clear();
    let (second, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, second, &RunOptions::default(), no_cancel(), &sink).unwrap();
    let seen = order.lock().unwrap().clone();
    assert!(seen[..baseline_runs].iter().all(|v| v == "none"), "the first {baseline_runs} runs to finish must be baseline runs: {seen:?}");
    assert!(seen[baseline_runs..].iter().all(|v| v != "none"));

    // All-pass in both arms is no headroom, not "no effect": no retire advice.
    let rep = report::build(&store, second, None).unwrap();
    let pass: Vec<_> = rep.comparisons.iter().filter(|c| c.metric == "pass").collect();
    assert!(!pass.is_empty() && pass.iter().all(|c| c.effect.verdict == stats::Verdict::NoHeadroom && c.effect.saturated_units == 3), "{pass:#?}");
    assert!(rep.warnings.iter().any(|w| w.contains("no headroom")), "{:?}", rep.warnings);
    assert!(rep.flaky_note.as_deref().unwrap_or("").contains("too few runs"));
    let adv = advice::build(&store, second, None).unwrap();
    let kinds: Vec<&str> = adv.iter().map(|s| s.kind.as_str()).collect();
    assert!(kinds.contains(&"no_headroom") && !kinds.contains(&"retire_pattern") && !kinds.contains(&"flaky_task"), "{kinds:?}");
    let nh = adv.iter().find(|s| s.kind == "no_headroom").unwrap();
    assert_eq!(nh.priority, 1);
    assert!(nh.action.contains("Every task passes without the pattern") && nh.action.contains("Add harder tasks"), "{}", nh.action);
    assert_eq!(nh.evidence["saturated_tasks"].as_array().unwrap().len(), 3);
}

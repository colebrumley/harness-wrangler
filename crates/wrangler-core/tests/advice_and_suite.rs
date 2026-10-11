mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wrangler_core::project::Suite;
use wrangler_core::scheduler::{self, RunOptions};
use wrangler_core::store::Annotation;
use wrangler_core::{advice, interop, review, suite, Store};

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn review_queue_votes_annotations_and_advice() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();

    // The queue is blind: an item names runs, never variants.
    let item = review::next(&store, launch).unwrap().expect("queue has items");
    let as_json = serde_json::to_string(&item).unwrap();
    assert!(!as_json.contains("overlay=") && !as_json.contains("plain") && !as_json.contains("noisy"), "review item leaks the variant");
    assert!(item.left.output.contains("QUALITY=") && item.progress.total_remaining > 0);
    let before = item.progress.total_remaining;

    let reveal = review::vote(&store, launch, item.left.run_id, item.right.run_id, "left", "tighter").unwrap();
    assert_ne!(reveal.left_variant, reveal.right_variant);
    let next = review::next(&store, launch).unwrap().unwrap();
    assert_eq!(next.progress.votes, 1);
    assert_eq!(next.progress.total_remaining, before - 1);
    assert_ne!((next.left.run_id, next.right.run_id), (item.left.run_id, item.right.run_id));
    // Skips leave the queue but do not count as votes; undo puts a pair back.
    let skip = review::vote(&store, launch, next.left.run_id, next.right.run_id, "skip", "").unwrap();
    assert_eq!(review::progress(&store, launch).unwrap().skipped, 1);
    store.delete_judgment(skip.judgment_id).unwrap();
    assert_eq!(review::progress(&store, launch).unwrap().total_remaining, before - 1);
    assert!(review::vote(&store, launch, item.left.run_id, 999_999, "left", "").is_err());

    // Vote the queue down: decided comparisons stop being "useful".
    let start_useful = review::progress(&store, launch).unwrap().useful_remaining;
    for _ in 0..40 {
        let Some(it) = review::next(&store, launch).unwrap() else { break };
        let q = |s: &str| s.split("QUALITY=").nth(1).and_then(|x| x[..5].parse::<f64>().ok()).unwrap_or(0.0);
        let choice = if q(&it.left.output) >= q(&it.right.output) { "left" } else { "right" };
        review::vote(&store, launch, it.left.run_id, it.right.run_id, choice, "").unwrap();
    }
    assert!(review::progress(&store, launch).unwrap().useful_remaining < start_useful);

    // Annotations: the same phrase flagged in two runs of one variant.
    let noisy: Vec<_> = store.runs(launch).unwrap().into_iter().filter(|r| r.variant_key == "overlay=noisy|effort=medium").take(2).collect();
    for r in &noisy {
        store
            .add_annotation(&Annotation { id: 0, run_id: r.id, kind: "comment".into(), target: "output".into(), line_start: 3, line_end: 3, col_start: Some(0), col_end: Some(7), quote: "Perhaps status".into(), body: "hedging".into(), created_at: String::new() })
            .unwrap();
    }
    store
        .add_annotation(&Annotation { id: 0, run_id: noisy[0].id, kind: "emoji".into(), target: "output".into(), line_start: 1, line_end: 1, col_start: None, col_end: None, quote: "# Summary".into(), body: "👎".into(), created_at: String::new() })
        .unwrap();
    let digest = review::annotation_digest(&store, launch).unwrap();
    let nv = digest.iter().find(|d| d.variant_key == "overlay=noisy|effort=medium").unwrap();
    assert_eq!(nv.passages[0].runs, 2);
    assert_eq!(nv.passages[0].comments, vec!["hedging", "hedging"]);
    assert_eq!(nv.emoji["👎"], 1);

    let suggestions = advice::build(&store, launch, None).unwrap();
    let kinds: Vec<&str> = suggestions.iter().map(|s| s.kind.as_str()).collect();
    assert!(kinds.contains(&"drop_pattern") && kinds.contains(&"keep_pattern") && kinds.contains(&"reviewer_flag"), "{kinds:?}");
    let drop = suggestions.iter().find(|s| s.kind == "drop_pattern").unwrap();
    assert!(drop.action.contains("noisy") && drop.action.contains("lowers judge score"), "{}", drop.action);
    assert!(drop.evidence["ci_high"].as_f64().unwrap() < 0.0 && drop.evidence["launch_id"] == launch);
    assert_eq!(drop.metric, "judge_score");
    // Suggestions are ordered and carry stable ids.
    assert!(suggestions.windows(2).all(|w| w[0].priority <= w[1].priority));
    assert!(suggestions[0].id.starts_with(&format!("L{launch}-")));

    // Human votes now feed the pairwise report alongside the judge.
    let rep = wrangler_core::report::build(&store, launch, None).unwrap();
    let pw = rep.pairwise.unwrap();
    assert!(pw.rankings.iter().any(|r| r.source == "human") && pw.rankings.iter().any(|r| r.source == "all"));
    assert!(pw.human_judge_overlap > 0);

    // benchmark.json export in the skill-creator shape.
    let bench = interop::export_benchmark(&store, launch, Some("plain · medium"), Some("none · medium")).unwrap();
    assert_eq!(bench["runs"].as_array().unwrap().len(), 24);
    assert_eq!(bench["runs"][0]["configuration"], "with_skill");
    assert!(bench["run_summary"]["with_skill"]["pass_rate"]["mean"].as_f64().unwrap() > bench["run_summary"]["without_skill"]["pass_rate"]["mean"].as_f64().unwrap());
    assert!(bench["run_summary"]["delta"]["pass_rate"].as_str().unwrap().starts_with('+'));
}

#[test]
fn suite_baselines_and_new_model_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    // A skill that helps the weaker model but is redundant on the stronger
    // one cannot be faked by the mock runner, so check the plumbing: both
    // baselines exist, deltas are computed, and categories are assigned.
    project.save_suite(&Suite { name: "s".into(), description: String::new(), primary_metric: "judge_score".into(), experiments: vec!["demo".into()] }).unwrap();
    assert!(suite::diff(&project, &store, "s", "mock-sonnet", None).is_err(), "no baseline yet");
    for model in ["mock-sonnet", "mock-opus"] {
        let launches = suite::run_model(&project, &store, "s", model, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();
        assert_eq!(launches.len(), 1);
        let l = store.launch(launches[0]).unwrap();
        assert_eq!(l.suite.as_deref(), Some("s"));
        assert!(l.variants.iter().all(|v| v.model.as_deref() == Some(model)));
    }
    let dash = suite::dashboard(&project, &store, "s").unwrap();
    assert_eq!(dash.models, vec!["mock-sonnet", "mock-opus"]);
    assert_eq!(dash.rows.len(), 2, "plain and noisy");
    let plain = dash.rows.iter().find(|r| r.pattern == "plain").unwrap();
    assert_eq!(plain.cells.len(), 2);
    assert_eq!(plain.cells[0].verdict, "helping");
    assert!(plain.cells[1].change_vs_prev.is_some() && plain.cells[1].prev_model.as_deref() == Some("mock-sonnet"));
    assert!(plain.cells[0].cost_effect.is_some());

    let d = suite::diff(&project, &store, "s", "mock-opus", None).unwrap();
    assert_eq!(d.prev_model.as_deref(), Some("mock-sonnet"));
    assert_eq!(d.items[0].pattern, "noisy", "the harming pattern is first");
    assert_eq!(d.items[0].category, "harming", "it hurt on the previous model too, so it is not a regression");
    assert_eq!(d.items.last().unwrap().category, "earning");
    assert!(suite::render_diff_text(&d).contains("HARMING") && suite::render_dashboard_text(&dash).contains("helping"));
    // Rerunning the older model does not make it the "new" one.
    suite::run_model(&project, &store, "s", "mock-sonnet", &RunOptions::default(), no_cancel(), &|_| {}).unwrap();
    assert_eq!(suite::dashboard(&project, &store, "s").unwrap().models, vec!["mock-sonnet", "mock-opus"]);
}

#[test]
fn imports_skill_creator_evals() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let skill = tmp.path().join("ext/my-skill");
    common::write(&skill, "evals/files/in.txt", "input");
    common::write(
        &skill,
        "evals/evals.json",
        r#"{"skill_name": "My Skill", "evals": [
            {"id": 1, "prompt": "Do the thing with in.txt", "expected_output": "It is done", "files": ["evals/files/in.txt"], "expectations": ["The output includes X"]},
            {"id": 2, "prompt": "No files here"}
        ]}"#,
    );
    let written = interop::import_skill_creator(&project, &skill.join("evals/evals.json"), None).unwrap();
    assert_eq!(written, vec!["tasks/my-skill/1.toml", "tasks/my-skill/2.toml"]);
    let t = project.load_task_file(&project.abs(&written[0])).unwrap();
    assert_eq!(t.id, "my-skill-1");
    assert_eq!(t.expectations, vec!["The output includes X"]);
    assert_eq!(std::fs::read_to_string(project.abs("fixtures/my-skill-1/in.txt")).unwrap(), "input");
    assert!(project.load_task_file(&project.abs(&written[1])).unwrap().fixture.is_none());
}

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wrangler_core::project::Suite;
use wrangler_core::scheduler::{self, RunOptions};
use wrangler_core::store::Annotation;
use wrangler_core::model::DetSpec;
use wrangler_core::{advice, interop, report, review, suite, Project, Store};

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn review_queue_asks_about_each_task_once() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();

    // The starter set is one pair per task, however many pairs there are.
    let start = review::progress(&store, launch).unwrap();
    assert_eq!((start.recommended, start.recommended_left), (3, 3));
    assert!(start.total_remaining > 3);

    // A skipped task goes to the back of the line and still owes a vote.
    let first = review::next(&store, launch).unwrap().unwrap();
    review::vote(&store, launch, first.left.run_id, first.right.run_id, "skip", "").unwrap();
    let mut seen = vec![];
    for left in [3, 2] {
        let it = review::next(&store, launch).unwrap().unwrap();
        assert_eq!((it.task_votes, it.progress.recommended_left), (0, left));
        assert_ne!(it.task_id, first.task_id);
        review::vote(&store, launch, it.left.run_id, it.right.run_id, "left", "").unwrap();
        seen.push(it.task_id);
    }
    assert_ne!(seen[0], seen[1]);
    let last = review::next(&store, launch).unwrap().unwrap();
    assert_eq!((last.task_id.as_str(), last.task_votes, last.progress.recommended_left), (first.task_id.as_str(), 0, 1));

    // Voting twice on one pair leaves one vote.
    review::vote(&store, launch, last.left.run_id, last.right.run_id, "left", "").unwrap();
    review::vote(&store, launch, last.right.run_id, last.left.run_id, "tie", "").unwrap();
    let done = review::progress(&store, launch).unwrap();
    assert_eq!((done.votes, done.recommended, done.recommended_left), (3, 3, 0));

    // Past the starter set every pair repeats a task, and the item says so.
    let more = review::next(&store, launch).unwrap().unwrap();
    assert_eq!(more.task_votes, 1);
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
    // With two factors, the first pair served isolates the pattern: same effort, with and without an overlay.
    {
        let key = |id: i64| store.run(id).unwrap().variant_key;
        let (l, r) = (key(item.left.run_id), key(item.right.run_id));
        let part = |k: &str, i: usize| k.split('|').nth(i).unwrap().to_string();
        assert_eq!(part(&l, 1), part(&r, 1), "effort should match: {l} vs {r}");
        assert_ne!(part(&l, 0), part(&r, 0), "overlay should differ: {l} vs {r}");
    }

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
    let text = wrangler_core::report::render_text(&rep);
    let pw = rep.pairwise.unwrap();
    assert!(pw.rankings.iter().any(|r| r.source == "human") && pw.rankings.iter().any(|r| r.source == "all"));
    assert!(pw.human_judge_overlap > 0);
    // The reviewer's one-line reason travels with the vote, in the report and its text form.
    let why = pw.human_votes.iter().find(|v| v.reason == "tighter").expect("vote reason is reported");
    assert_ne!(why.a, why.b);
    assert!(text.contains(": tighter\n"));

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

/// Trigger experiments: tasks marked `should` / `should_not` become
/// `skill_invoked` checks, and the report scores the skill as a classifier.
#[test]
fn trigger_corpus_precision_recall_and_advice() {
    let tmp = tempfile::tempdir().unwrap();
    let project = Project::init(tmp.path()).unwrap();
    common::write(tmp.path(), "overlays/notes/.claude/skills/notes-dir/SKILL.md", "---\nname: release-notes\ndescription: Use when drafting release notes. Also for changelog entries. Mentions banana often.\n---\n# Notes\n\nmock-effect: 0.1\n");
    let task = |id: &str, prompt: &str, trigger: &str| common::write(tmp.path(), &format!("tasks/trigger/{id}.toml"), &format!("id = \"{id}\"\nprompt = \"{prompt}\"\ntrigger = \"{trigger}\"\nmax_turns = 2\n"));
    task("s1", "Write the release notes for 2.0", "should");
    task("s2", "Add a changelog line for the fix", "should");
    task("s3", "Tell users what shipped this week", "should"); // no word from the description: a miss
    task("n1", "How long is a typical press statement?", "should_not");
    task("n2", "Peel a banana and describe it", "should_not"); // fires on "banana": an overfire
    common::write(tmp.path(), "tasks/plain.toml", "id = \"plain\"\nprompt = \"p\"\n");
    common::write(
        tmp.path(),
        "experiments/trig.toml",
        "name = \"trig\"\nruns_per_cell = 3\n[runner]\nkind = \"mock\"\n[trigger]\nskill = \"release-notes\"\n[[factors]]\nname = \"overlay\"\nkind = \"overlay\"\nlevels = [\"overlays/none\", \"overlays/notes\"]\n[tasks]\ninclude = [\"tasks/trigger/*.toml\"]\n",
    );
    let exp = project.load_experiment("trig").unwrap();
    // Expansion: one skill_invoked grader per trigger task, with the matching expectation.
    let tasks = project.load_tasks(&exp).unwrap();
    let expect = |id: &str| match &tasks.iter().find(|t| t.id == id).unwrap().graders.deterministic[..] {
        [DetSpec::SkillInvoked { skill: Some(s), expect, name: Some(n) }] if s == "release-notes" && n == "trigger:release-notes" => *expect,
        other => panic!("{id}: {other:?}"),
    };
    assert!(expect("s1") && expect("s3") && !expect("n1") && !expect("n2"));
    assert!(project.validate(&exp).unwrap().iter().all(|w| !w.contains("never fire")));

    let store = Store::open(&project.db_path()).unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let rep = report::build(&store, launch, None).unwrap();
    assert_eq!(rep.primary_metric, "trigger_f1");
    let t = rep.trigger.as_ref().expect("trigger section");
    let (none, with) = (&t.variants[0], &t.variants[1]);
    // Without the skill nothing fires: precision undefined, recall zero.
    assert!(none.precision.is_none() && none.recall.as_ref().unwrap().mean == 0.0 && none.description.is_none());
    assert_eq!((with.should_tasks, with.should_not_tasks), (3, 2));
    assert!((with.recall.as_ref().unwrap().mean - 2.0 / 3.0).abs() < 1e-9, "{:?}", with.recall);
    assert!((with.precision.as_ref().unwrap().mean - 2.0 / 3.0).abs() < 1e-9, "{:?}", with.precision);
    assert_eq!(with.false_negatives.iter().map(|p| p.task_id.as_str()).collect::<Vec<_>>(), vec!["s3"]);
    // A run that fires on a should-not task is a false positive.
    assert_eq!(with.false_positives.iter().map(|p| (p.task_id.as_str(), p.fire_rate)).collect::<Vec<_>>(), vec![("n2", 1.0)]);
    assert!(with.description.as_deref().unwrap().starts_with("Use when drafting release notes."));
    let f1 = rep.comparisons.iter().find(|c| c.metric == "trigger_f1" && c.level == "notes").expect("trigger_f1 is a comparable metric");
    assert!(f1.effect.effect > 0.6 && f1.effect.units == 5, "{:?}", f1.effect);
    let text = report::render_text(&rep);
    assert!(text.contains("TRIGGER (release-notes") && text.contains("fired on 1 should-not prompt(s)"), "{text}");
    // Reaching a task's own turn cap is a normal end, and each trigger run passes or fails on the trigger.
    assert!(store.runs(launch).unwrap().iter().all(|r| r.status == "done"));

    let adv = advice::build(&store, launch, None).unwrap();
    let miss = adv.iter().find(|s| s.kind == "trigger_misses").expect("trigger_misses");
    assert_eq!(miss.evidence["missed_prompts"][0]["prompt"], "Tell users what shipped this week");
    assert!(miss.evidence["description"].as_str().unwrap().contains("changelog"));
    let over = adv.iter().find(|s| s.kind == "trigger_overfires").expect("trigger_overfires");
    assert_eq!(over.evidence["fired_prompts"][0]["task"], "n2");
    assert!(over.evidence["description"].as_str().unwrap().contains("banana"));
    assert!(adv.iter().all(|s| s.kind != "fix_trigger"));

    // Description ablation: removing the banana sentence restores precision, and advice names it.
    let plan = wrangler_core::ablation::generate(&project, &wrangler_core::ablation::AblationRequest { source: "overlays/notes".into(), description_sentences: true, tasks: vec!["tasks/trigger/*.toml".into()], runs_per_cell: Some(3), runner: Some("mock".into()), ..Default::default() }).unwrap();
    let exp = project.load_experiment(&plan.experiment).unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let rep = report::build(&store, launch, None).unwrap();
    let ab = rep.ablation.as_ref().unwrap();
    assert_eq!(ab.mode, "description_sentences");
    let banana = ab.sections.iter().find(|s| s.title.contains("banana")).unwrap();
    assert!(banana.precision_effect.as_ref().unwrap().effect < -0.2, "the banana sentence costs precision: {:?}", banana.precision_effect);
    let changelog = ab.sections.iter().find(|s| s.title.contains("changelog")).unwrap();
    assert!(changelog.recall_effect.as_ref().unwrap().effect > 0.2, "the changelog sentence carries recall: {:?}", changelog.recall_effect);
    let adv = advice::build(&store, launch, None).unwrap();
    let over = adv.iter().find(|s| s.kind == "trigger_overfires" && s.target["variant"] == "full").expect("overfire advice for the full skill");
    assert!(over.evidence["responsible_sentence"]["text"].as_str().unwrap().contains("banana"), "{}", over.action);
    assert!(report::render_ablation_text(ab).contains("precision"));
}

/// The example project's hand-written rfw trigger corpus gives the mock
/// runner something to get wrong in both directions.
#[test]
fn example_rfw_trigger_corpus_is_nontrivial_under_the_mock() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let tmp = tempfile::tempdir().unwrap();
    let project = Project::init(tmp.path()).unwrap();
    for dir in ["tasks/trigger/reader-first-writing", "overlays/rfw-v1.1.0"] {
        wrangler_core::workspace::copy_dir_contents(&examples.join(dir), &{
            let d = tmp.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            d
        })
        .unwrap();
    }
    let mut exp = wrangler_core::Experiment::parse(&std::fs::read_to_string(examples.join("experiments/rfw-trigger.toml")).unwrap()).unwrap();
    exp.runner.kind = "mock".into();
    let tasks = project.load_tasks(&exp).unwrap();
    let count = |should: bool| tasks.iter().filter(|t| t.should_trigger() == Some(should)).count();
    assert!(count(true) >= 6 && count(false) >= 6, "at least six prompts of each kind");
    assert!(tasks.iter().all(|t| t.max_turns == Some(2) && t.fixture.is_none()));
    let store = Store::open(&project.db_path()).unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let rep = report::build(&store, launch, None).unwrap();
    let t = rep.trigger.unwrap();
    for v in &t.variants {
        let (p, r) = (v.precision.as_ref().unwrap().mean, v.recall.as_ref().unwrap().mean);
        assert!(p > 0.3 && p < 1.0 && r > 0.3 && r < 1.0, "precision {p} and recall {r} should both be non-trivial");
        assert!(!v.false_negatives.is_empty() && !v.false_positives.is_empty());
    }
    assert_eq!(t.by_model.len(), 2, "one row per model");
}

/// Judge calibration: scripted human votes agree with the judge on two tags
/// and disagree on a third. The report shows the split, the calibration
/// queue serves judged pairs first, and advice is downgraded where the
/// judge is not trusted.
#[test]
fn judge_calibration_by_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    for (id, tag) in [("alpha", "summaries"), ("bravo", "emails"), ("charlie", "code")] {
        let path = tmp.path().join(format!("tasks/{id}.toml"));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen("expected_output", &format!("tags = [\"{tag}\"]\nexpected_output"), 1)).unwrap();
    }
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();

    // Before any human vote: nothing to calibrate on, and advice says how many pairs it takes.
    let rep = report::build(&store, launch, None).unwrap();
    assert_eq!(rep.calibration.as_ref().unwrap().overall.status, "not_enough");
    let adv = advice::build(&store, launch, None).unwrap();
    let cal = adv.iter().find(|s| s.kind == "calibrate_judge").expect("calibrate_judge");
    assert_eq!((cal.priority, cal.evidence["pairs_needed"].as_u64()), (2, Some(24)), "{}", cal.action);
    assert!(adv.iter().filter(|s| s.metric == "judge_score").all(|s| s.action.contains("The judge is not calibrated on these tasks")));

    let judge_votes = store.judgments(launch, Some("judge")).unwrap();
    let judged = |a: i64, b: i64| judge_votes.iter().find(|j| (j.run_a, j.run_b) == (a, b) || (j.run_a, j.run_b) == (b, a));
    let tag_of = |task: &str| match task {
        "alpha" => "summaries",
        "bravo" => "emails",
        _ => "code",
    };
    let mut offered = 0;
    for _ in 0..200 {
        let c = wrangler_core::calibration::for_launch(&store, launch).unwrap();
        if ["summaries", "emails", "code"].iter().all(|t| c.tag(t).map(|r| r.pairs >= 8).unwrap_or(false)) {
            break;
        }
        let item = review::next_for(&store, launch, review::Mode::Calibration).unwrap().expect("pairs remain");
        // Calibration mode never offers an unjudged pair while a judged one remains.
        let j = judged(item.left.run_id, item.right.run_id).unwrap_or_else(|| panic!("pair {} of the calibration queue has no judge vote", offered + 1));
        assert!(item.why.iter().any(|w| w == "calibration"));
        offered += 1;
        let judge_side = match (j.winner.as_str(), j.run_a == item.left.run_id) {
            ("a", true) | ("b", false) => "left",
            ("b", true) | ("a", false) => "right",
            _ => "tie",
        };
        let agree = tag_of(&item.task_id) != "code";
        let choice = match (judge_side, agree) {
            ("tie", true) => "tie",
            ("tie", false) => "left",
            (side, true) => side,
            ("left", false) => "right",
            (_, false) => "left",
        };
        review::vote(&store, launch, item.left.run_id, item.right.run_id, choice, "").unwrap();
    }
    assert!(offered >= 24, "the queue spread over the three tags: {offered} votes");

    let rep = report::build(&store, launch, None).unwrap();
    let c = rep.calibration.as_ref().unwrap();
    let (s, e, code) = (c.tag("summaries").unwrap(), c.tag("emails").unwrap(), c.tag("code").unwrap());
    assert_eq!((s.status.as_str(), s.agreement, e.status.as_str(), e.agreement), ("calibrated", Some(1.0), "calibrated", Some(1.0)));
    assert_eq!((code.status.as_str(), code.agreement), ("low", Some(0.0)));
    assert!(code.kappa.unwrap() < 0.0 && s.kappa.unwrap() > 0.9);
    assert!(s.rubric_pairs > 0 && s.rubric_agreement.unwrap() > code.rubric_agreement.unwrap());
    let text = report::render_text(&rep);
    assert!(text.contains("JUDGE CALIBRATION") && text.contains("low agreement"), "{text}");

    // Suggestions about one tag's tasks follow that tag's calibration: only `code` is downgraded.
    let about = |task: &str| advice::Suggestion {
        id: String::new(),
        kind: "flaky_task".into(),
        priority: 6,
        action: format!("Task {task} is flaky."),
        target: serde_json::json!({"type": "tasks", "tasks": [task]}),
        metric: "judge_score".into(),
        evidence: serde_json::json!({}),
        confidence: "high".into(),
    };
    let mut scoped = vec![about("alpha"), about("bravo"), about("charlie")];
    scoped.push(advice::Suggestion { metric: "pass".into(), ..about("charlie") });
    let tasks = store.launch(launch).unwrap().tasks;
    advice::apply_calibration(&mut scoped, c, &tasks);
    assert_eq!(scoped.iter().map(|s| s.confidence.as_str()).collect::<Vec<_>>(), vec!["high", "high", "medium", "high"]);
    assert!(scoped[2].action.contains("The judge is not calibrated on these tasks tagged `code` (agreement 0%"), "{}", scoped[2].action);
    assert_eq!(scoped[0].action, "Task alpha is flaky.");
    assert_eq!(scoped[2].evidence["judge_calibration"]["untrusted"], serde_json::json!(["code"]));

    let adv = advice::build(&store, launch, None).unwrap();
    let cal = adv.iter().find(|s| s.kind == "calibrate_judge").expect("calibrate_judge names the untrusted tag");
    assert!(cal.action.contains("on `code` tasks") && !cal.action.contains("`emails`"), "{}", cal.action);

    // Pooled across launches by judge model, with a tag filter.
    let pooled = wrangler_core::calibration::pooled(&store, Some("mock"), None, Some("code")).unwrap();
    assert_eq!((pooled.len(), pooled[0].launches.clone(), pooled[0].calibration.overall.status.as_str()), (1, vec![launch], "low"));
    assert!(wrangler_core::calibration::pooled(&store, Some("other-model"), None, None).unwrap().is_empty());

    // A suite baseline carries the calibration it was taken under.
    project.save_suite(&Suite { name: "cal".into(), description: String::new(), primary_metric: "judge_score".into(), experiments: vec!["demo".into()] }).unwrap();
    store.set_baseline("cal", "mock-model", "demo", launch, &serde_json::Value::Null).unwrap();
    let dash = suite::dashboard(&project, &store, "cal").unwrap();
    assert_eq!(dash.rows[0].cells[0].judge_calibration["status"], "low");
    assert_eq!(store.baselines("cal").unwrap()[0].calibration["untrusted_tags"], serde_json::json!(["code"]));
}

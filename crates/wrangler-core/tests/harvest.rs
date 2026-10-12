//! Task harvesting, against a recorded Claude Code 2.1.296 session
//! (`fixtures/session.jsonl`, scrubbed: paths and names replaced, token
//! counts and thinking removed, everything harvesting reads left as it was).

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wrangler_core::harvest::{self, capture_fixture, FixtureOptions, Session, SessionImport};
use wrangler_core::model::DetSpec;
use wrangler_core::scheduler::{self, RunOptions};
use wrangler_core::{report, Project, Store};

const SESSION: &str = include_str!("fixtures/session.jsonl");
const SESSION_CWD: &str = "/Users/dev/orders";

fn git(dir: &Path, date: &str, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=dev", "-c", "user.email=dev@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap();
    assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
}

/// The repository the recorded session worked in: one commit from before the
/// session, the session's own fix committed after it, a secret, an ignored
/// directory, and a file that was never committed.
fn repo(root: &Path) -> std::path::PathBuf {
    let dir = root.join("orders");
    common::write(&dir, "slugify.py", "def slugify(text):\n    return text.replace(' ', '-')\n");
    common::write(&dir, "tests/test_slugify.py", "import unittest\n");
    common::write(&dir, ".gitignore", "__pycache__/\n");
    common::write(&dir, ".env", "API_TOKEN=not-a-real-token\n");
    common::write(&dir, "deploy/server.pem", "-----BEGIN-----\n");
    common::write(&dir, ".env.example", "API_TOKEN=\n");
    git(&dir, "2026-10-01T10:00:00Z", &["init", "-q", "-b", "main"]);
    git(&dir, "2026-10-01T10:00:00Z", &["add", "-A", "-f"]);
    git(&dir, "2026-10-01T10:00:00Z", &["commit", "-q", "-m", "before the session"]);
    // After the session (it began 2026-10-12): its fix, committed.
    common::write(&dir, "slugify.py", "def slugify(text):\n    return text.lower().replace(' ', '-')\n");
    common::write(&dir, "NOTES.md", "slugify did not lowercase.\n");
    git(&dir, "2026-10-20T10:00:00Z", &["add", "slugify.py", "NOTES.md"]);
    git(&dir, "2026-10-20T10:00:00Z", &["commit", "-q", "-m", "the session's fix"]);
    common::write(&dir, "__pycache__/x.pyc", "junk");
    common::write(&dir, "local-notes.md", "uncommitted, as it is after the session\n");
    dir.canonicalize().unwrap()
}

/// The recorded session, moved to a real directory, with one more edit: to a
/// file that was never committed.
fn session_in(dir: &Path) -> String {
    let moved = SESSION.replace(SESSION_CWD, &dir.to_string_lossy());
    let edit = serde_json::json!({"type": "assistant", "isSidechain": false, "cwd": dir, "sessionId": "18fc056f-264b-4b94-82e2-5955ab69adb8",
        "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t-extra", "name": "Edit", "input": {"file_path": dir.join("local-notes.md"), "old_string": "a", "new_string": "b"}}]}});
    format!("{moved}{edit}\n")
}

#[test]
fn recorded_session_parses() {
    let s = Session::parse(SESSION).unwrap();
    assert_eq!((s.cwd.as_str(), s.version.as_str(), s.git_branch.as_str()), (SESSION_CWD, "2.1.296", "main"));
    assert_eq!(s.id, "18fc056f-264b-4b94-82e2-5955ab69adb8");
    assert!(s.started_at.starts_with("2026-10-12T"));
    // Prompt extraction by turn: the person's two messages, and nothing the harness wrote.
    let prompts = s.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[0].1.starts_with("The tests in tests/ are failing. Run them with python3 -m unittest"));
    assert_eq!(prompts[1].1, "Thanks, that works. Also add a one-line NOTES.md saying what was wrong.");
    // Test-command recognition: the session ran unittest twice, in longer shell lines.
    assert_eq!(s.test_commands(prompts[0].0), vec!["python3 -m unittest discover -s tests -q"]);
    assert!(s.test_commands(prompts[1].0).is_empty(), "no tests ran after the second message");
    // What it changed: one Edit to an existing file, one Write of a new one.
    assert_eq!(s.touched_files(), vec![("slugify.py".to_string(), true), ("NOTES.md".to_string(), false)]);
    assert!(s.diff_lines(prompts[0].0) >= 3 && s.diff_lines(prompts[1].0) == 1);
    assert_eq!(s.revealed_head(), None, "this session ran no git command");
    let digest = s.digest(prompts[0].0, 10_000);
    assert!(digest.contains("[agent ran Edit]") && digest.contains("[user] Thanks, that works."));
}

#[test]
fn fixture_is_the_commit_the_session_started_from() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = repo(tmp.path());
    let project = Project::init(&tmp.path().join("proj")).unwrap();
    let session_file = tmp.path().join("session.jsonl");
    std::fs::write(&session_file, session_in(&dir)).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));

    let h = harvest::import_session(&project, &session_file, &SessionImport::default(), &cancel).unwrap();
    assert_eq!(h.task_id, "orders-18fc056f");
    let f = h.fixture.as_ref().unwrap();
    assert_eq!((f.source.as_str(), f.commit_from.as_deref()), ("commit", Some("date")));
    let fx = project.abs(&f.dir);
    // The state before the session: the bug is still there, the session's NOTES.md is not.
    assert_eq!(std::fs::read_to_string(fx.join("slugify.py")).unwrap(), "def slugify(text):\n    return text.replace(' ', '-')\n");
    assert!(fx.join("tests/test_slugify.py").exists() && !fx.join("NOTES.md").exists() && !fx.join("__pycache__").exists() && !fx.join(".git").exists());
    // Secrets are never copied, and their names are reported.
    assert!(!fx.join(".env").exists() && !fx.join("deploy/server.pem").exists() && !fx.join(".env.example").exists());
    assert_eq!(f.secrets_skipped, vec![".env", ".env.example", "deploy/server.pem"]);
    // A file the session edited that the commit does not have is reported and copied as it is now.
    assert_eq!(f.working_tree_files, vec!["local-notes.md"]);
    assert!(fx.join("local-notes.md").exists());
    assert!(h.warnings.iter().any(|w| w.contains("uncommitted when it started") && w.contains("local-notes.md")), "{:?}", h.warnings);
    assert!(h.warnings.iter().any(|w| w.contains("last commit before the session began")));

    // The task: prompt, fixture, proposed graders, tags, and a source comment.
    let text = std::fs::read_to_string(project.abs(&h.task_file)).unwrap();
    assert!(text.starts_with("# source: Claude Code session 18fc056f-264b-4b94-82e2-5955ab69adb8, turn 1, 2026-10-12 (Claude Code 2.1.296)\n"), "{text}");
    let task = project.load_task_file(&project.abs(&h.task_file)).unwrap();
    assert!(task.prompt.starts_with("The tests in tests/ are failing."));
    assert_eq!(task.fixture.as_ref().unwrap().dir.as_deref(), Some("fixtures/orders-18fc056f"));
    assert_eq!(task.tags, vec!["harvested", "draft", "orders"]);
    assert!(task.expectations.is_empty() && task.expected_output.is_empty(), "no model-written expectations without the flag");
    match &task.graders.deterministic[..] {
        [DetSpec::Command { cmd, name: Some(n), .. }, DetSpec::DiffSize { max: Some(max), .. }] => {
            assert_eq!((cmd.as_str(), n.as_str()), ("python3 -m unittest discover -s tests -q", "tests-pass"));
            assert_eq!(Some(*max), h.diff_max);
            assert!(*max >= 10);
        }
        other => panic!("{other:?}"),
    }

    // Harvesting twice under the same id refuses to overwrite.
    assert!(format!("{:#}", harvest::import_session(&project, &session_file, &SessionImport::default(), &cancel).unwrap_err()).contains("already exists"));
    // --turn picks the later message; --include lets one named file through; expectations only on request.
    let h2 = harvest::import_session(&project, &session_file, &SessionImport { turn: Some(2), include: vec![".env.example".into()], draft_expectations: Some("mock".into()), id: Some("second".into()), ..Default::default() }, &cancel).unwrap();
    assert!(h2.prompt.starts_with("Thanks, that works."));
    assert!(project.abs("fixtures/second/.env.example").exists() && !project.abs("fixtures/second/.env").exists());
    let t2 = project.load_task_file(&project.abs(&h2.task_file)).unwrap();
    assert!(!t2.expectations.is_empty() && !t2.expected_output.is_empty() && t2.tags.contains(&"draft".to_string()));
    assert!(h2.warnings.iter().any(|w| w.contains("--turn 2")));
    assert!(format!("{:#}", harvest::import_session(&project, &session_file, &SessionImport { turn: Some(3), ..Default::default() }, &cancel).unwrap_err()).contains("2 user message(s)"));
}

#[test]
fn size_cap_and_current_state_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = repo(tmp.path());
    let head = String::from_utf8(Command::new("git").current_dir(&dir).args(["rev-list", "--max-parents=0", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
    // Above the cap the import stops and says so, and nothing is copied.
    let small = FixtureOptions { max_bytes: 20, include: vec![] };
    let err = capture_fixture(&dir, Some((&head, "session")), &[], &tmp.path().join("too-big"), &small).unwrap_err();
    assert!(format!("{err:#}").contains("above the 0 MB cap") && !tmp.path().join("too-big").exists(), "{err:#}");
    // A commit named in the session is used as given.
    let rep = capture_fixture(&dir, Some((&head, "session")), &[], &tmp.path().join("by-sha"), &FixtureOptions::default()).unwrap();
    assert_eq!((rep.source.as_str(), rep.commit.as_deref(), rep.warnings.len()), ("commit", Some(head.as_str()), 0));
    assert!(!tmp.path().join("by-sha/NOTES.md").exists());

    // No usable commit: a copy of the directory as it is now, with a warning.
    let rep = capture_fixture(&dir, Some(("0000000000000000000000000000000000000000", "session")), &[], &tmp.path().join("now"), &FixtureOptions::default()).unwrap();
    assert_eq!(rep.source, "current");
    assert!(rep.warnings[0].contains("CURRENT state"));
    let now = tmp.path().join("now");
    assert!(now.join("NOTES.md").exists() && now.join("local-notes.md").exists(), "tracked and untracked files are copied");
    assert!(!now.join("__pycache__").exists() && !now.join(".env").exists() && !now.join(".git").exists(), "gitignored files and secrets are not");
    assert!(rep.secrets_skipped.contains(&".env".to_string()));
    // Not a git repository at all.
    let plain = tmp.path().join("plain");
    common::write(&plain, "a/b.txt", "x");
    common::write(&plain, "id_rsa", "key");
    let rep = capture_fixture(&plain, None, &[], &tmp.path().join("plain-copy"), &FixtureOptions::default()).unwrap();
    assert_eq!((rep.source.as_str(), rep.files, rep.secrets_skipped.clone()), ("current", 1, vec!["id_rsa".to_string()]));
}

/// A run becomes a task with the run's output as the judge's reference; the
/// task passes a baseline check and joins an experiment without edits.
#[test]
fn import_run_then_check_then_join() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let exp = project.load_experiment("demo").unwrap();
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    let no_cancel = || Arc::new(AtomicBool::new(false));
    scheduler::execute(&project, &store, launch, &RunOptions { skip_judge: true, ..Default::default() }, no_cancel(), &|_| {}).unwrap();
    let run = store.runs(launch).unwrap().into_iter().find(|r| r.task_id == "bravo" && r.variant_key.starts_with("overlay=plain")).unwrap();

    let h = harvest::import_run(&project, &store, run.id, None).unwrap();
    assert_eq!(h.task_file, format!("tasks/harvested/bravo-from-run-{}.toml", run.id));
    let text = std::fs::read_to_string(project.abs(&h.task_file)).unwrap();
    assert!(text.starts_with(&format!("# source: wrangler run {} (launch #{launch} of demo, task bravo, variant plain", run.id)), "{text}");
    let task = harvest::find_task(&project, &h.task_id).unwrap();
    assert_eq!(task.reference_output.as_deref(), Some(run.output_text.trim()));
    assert_eq!(task.fixture.as_ref().unwrap().dir.as_deref(), Some("fixtures/incident"));
    assert!(task.tags.contains(&"harvested".to_string()) && task.tags.contains(&"draft".to_string()));
    assert_eq!(task.graders.deterministic.len(), 2, "the original task's graders carry over");

    // task check: the task alone, on the baseline alone, borrowing the experiment's runner and judge.
    let check = harvest::check_experiment(&project, &task, &harvest::CheckOptions { reps: Some(3), experiment: Some("demo".into()), ..Default::default() }).unwrap();
    assert!(check.factors.is_empty() && check.tasks.include == vec![h.task_file.clone()] && check.runner.kind == "mock");
    assert_eq!(check.variants().len(), 1);
    let (id, _) = scheduler::create_launch(&project, &store, &check, None, "task check").unwrap();
    scheduler::execute(&project, &store, id, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();
    let rep = report::build(&store, id, None).unwrap();
    assert_eq!((rep.counts.done, rep.tasks.len(), rep.variants.len()), (3, 1, 1));
    assert!(rep.variants[0].metrics.contains_key("pass") && rep.variants[0].metrics.contains_key("judge_score"));
    assert!(rep.tasks[0].headroom_by_metric.contains_key("pass"));
    assert!(report::render_headroom_text(&rep).contains(&h.task_id));
    // Without --experiment the check uses defaults and still needs nothing else.
    let bare = harvest::check_experiment(&project, &task, &harvest::CheckOptions { runner: Some("mock".into()), ..Default::default() }).unwrap();
    project.validate(&bare).unwrap();

    // It joins the experiment as it is: demo selects tasks/*.toml, so widen the glob.
    let mut joined = exp.clone();
    joined.tasks.include = vec!["tasks/*.toml".into(), "tasks/harvested/*.toml".into()];
    assert_eq!(project.load_tasks(&joined).unwrap().len(), 4);
    project.validate(&joined).unwrap();
}

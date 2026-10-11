mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wrangler_core::scheduler::{self, Event, RunOptions};
use wrangler_core::{report, Store};

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn budget_stops_new_runs_and_resume_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let project = common::project(tmp.path());
    let store = Store::open(&project.db_path()).unwrap();
    let mut exp = project.load_experiment("demo").unwrap();
    assert!(wrangler_core::Experiment::parse("name='x'\nbudget_usd = 0\n").is_err());
    // Mock runs cost about $0.004 each; 72 are planned.
    exp.budget_usd = Some(0.05);
    exp.runner.concurrency = 2;
    let (launch, _) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    let warned = std::sync::Mutex::new(vec![]);
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &|e| {
        if let Event::Warning { message } = e {
            warned.lock().unwrap().push(message)
        }
    })
    .unwrap();
    let l = store.launch(launch).unwrap();
    assert_eq!(l.status, "budget");
    let runs = store.runs(launch).unwrap();
    let done = runs.iter().filter(|r| r.status == "done").count();
    let spent = scheduler::launch_cost(&store, launch).unwrap();
    assert!(done >= 5 && done < 40, "{done} runs finished");
    // Overshoot is bounded by the runs in flight when the cap was crossed.
    assert!(spent >= 0.05 && spent < 0.05 + 2.0 * 0.01, "spent {spent}");
    assert!(runs.iter().all(|r| r.status == "done" || r.status == "pending"), "nothing is left half-run");
    assert!(warned.lock().unwrap().iter().any(|m| m.contains("budget of $0.05 reached")));
    assert!(store.grades_for_launch(launch).unwrap().iter().all(|g| g.tier != "judge"), "the judge phase must not spend past the cap");
    let rep = report::build(&store, launch, None).unwrap();
    assert!(rep.warnings.iter().any(|w| w.contains("budget") && w.contains("--budget")), "{:?}", rep.warnings);

    // Resuming under the same cap starts nothing; a higher cap finishes the launch.
    scheduler::execute(&project, &store, launch, &RunOptions::default(), no_cancel(), &|_| {}).unwrap();
    assert_eq!(store.runs(launch).unwrap().iter().filter(|r| r.status == "done").count(), done);
    scheduler::execute(&project, &store, launch, &RunOptions { budget_usd: Some(50.0), ..Default::default() }, no_cancel(), &|_| {}).unwrap();
    assert_eq!(store.launch(launch).unwrap().status, "done");
    assert!(store.runs(launch).unwrap().iter().all(|r| r.status == "done"));
    assert_eq!(store.launch(launch).unwrap().def.budget_usd, Some(50.0));
}

/// A stand-in `claude` that reports where it ran and probes what it can see.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "kernel=$(uname -s) passed=$PASSED_IN secret_len=${#WRANGLER_DOCKER_TEST_KEY}" > SUMMARY.md
ls "$HOST_ONLY_DIR" >/dev/null 2>&1 && echo "host dir visible" >> SUMMARY.md
echo '{"type":"system","subtype":"init","model":"fake-model","mcp_servers":[]}'
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"text","text":"done"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.001,"result":"done","usage":{"input_tokens":5,"output_tokens":5}}'
"#;

#[test]
fn docker_isolation_runs_the_runner_in_a_container() {
    if !wrangler_core::isolation::docker_available() {
        eprintln!("skipped: no Docker daemon is running");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = common::project(&root.join("proj"));
    let store = Store::open(&project.db_path()).unwrap();
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join("claude"), FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(tools.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let host_only = root.join("host-only");
    std::fs::create_dir_all(&host_only).unwrap();
    std::env::set_var("WRANGLER_CLAUDE_BIN", "/tools/claude");
    std::env::set_var("WRANGLER_DOCKER_TEST_KEY", "abcdef");

    let toml = |image: &str| {
        format!(
            "name = 'dock'\nruns_per_cell = 1\nisolation = 'docker'\n[runner]\nkind = 'claude-code'\nconcurrency = 1\n{image}docker_env = ['WRANGLER_DOCKER_TEST_KEY']\ndocker_args = ['-v', '{}:/tools:ro', '-e', 'PASSED_IN=yes', '-e', 'HOST_ONLY_DIR={}']\n[[factors]]\nname = 'overlay'\nkind = 'overlay'\nlevels = ['overlays/plain']\n[tasks]\ninclude = ['tasks/alpha.toml']\n",
            tools.display(),
            host_only.display()
        )
    };
    // Without an image the experiment is rejected before anything runs.
    let err = project.validate(&wrangler_core::Experiment::parse(&toml("")).unwrap()).unwrap_err();
    assert!(format!("{err:#}").contains("docker_image"));

    let exp = wrangler_core::Experiment::parse(&toml("docker_image = 'alpine:3.20'\n")).unwrap();
    let (launch, warnings) = scheduler::create_launch(&project, &store, &exp, None, "").unwrap();
    assert!(warnings.iter().any(|w| w.contains("keychain")), "{warnings:?}");
    scheduler::execute(&project, &store, launch, &RunOptions { skip_judge: true, ..Default::default() }, no_cancel(), &|_| {}).unwrap();
    let run = &store.runs(launch).unwrap()[0];
    assert_eq!(run.status, "done", "{}\n{}", run.error, std::fs::read_to_string(std::path::Path::new(&run.dir).join("stderr.txt")).unwrap_or_default());
    // It ran on Linux, in the mounted workspace, with the env passed by name and by value.
    assert_eq!(run.output_text.trim(), "kernel=Linux passed=yes secret_len=6");
    assert!(!run.output_text.contains("host dir visible"), "host paths outside the workspace must not be mounted");
    let events = run.events.to_string();
    assert!(events.contains("ran in docker container wrangler-"), "{events}");
    // The secret's value never reaches the recorded command line.
    let meta = std::fs::read_to_string(std::path::Path::new(&run.dir).join("meta.json")).unwrap();
    assert!(!meta.contains("abcdef"));
    // The overlay was materialized on the host and the diff still shows only the agent's work.
    let diff = std::fs::read_to_string(std::path::Path::new(&run.dir).join("diff.patch")).unwrap();
    assert!(diff.contains("SUMMARY.md") && !diff.contains("SKILL.md"));
    // No container is left behind.
    let left = std::process::Command::new("docker").args(["ps", "-a", "--filter", "name=wrangler-", "--format", "{{.Names}}"]).output().unwrap();
    assert!(String::from_utf8_lossy(&left.stdout).trim().is_empty());
}

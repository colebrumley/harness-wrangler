//! The rate-limit path end to end, against a stand-in `claude` binary that
//! rejects the first two attempts with a 429 and then succeeds. Its own
//! file because it sets process-wide environment variables.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use wrangler_core::scheduler::{self, Event, RunOptions};
use wrangler_core::Store;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
# Counts attempts per workspace; the workspace is reset between retries, so the counter lives beside it.
count_file="$FAKE_STATE/$(basename "$PWD")"
n=$(cat "$count_file" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$count_file"
echo '{"type":"system","subtype":"init","model":"fake-model","mcp_servers":[]}'
if [ "$n" -le "$FAKE_REJECTIONS" ]; then
  echo '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1791757200}}'
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"api_error_status":429,"num_turns":0,"result":"Claude usage limit reached"}'
  exit 1
fi
echo partial > leftover-from-attempt-$n.txt
echo "ok on attempt $n" > SUMMARY.md
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"text","text":"Wrote SUMMARY.md"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.001,"result":"Wrote SUMMARY.md","usage":{"input_tokens":5,"output_tokens":5}}'
"#;

#[test]
fn rate_limits_are_retried_with_backoff_and_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = common::project(&root.join("proj"));
    let bin = root.join("fake-claude");
    std::fs::write(&bin, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::env::set_var("WRANGLER_CLAUDE_BIN", &bin);
    std::env::set_var("WRANGLER_BACKOFF_BASE_MS", "40");
    std::env::set_var("FAKE_STATE", root.join("state"));

    let exp = |name: &str, retries: u32| {
        wrangler_core::Experiment::parse(&format!(
            "name = '{name}'\nruns_per_cell = 1\n[runner]\nkind = 'claude-code'\nconcurrency = 1\nmax_retries = {retries}\n[tasks]\ninclude = ['tasks/alpha.toml']\n"
        ))
        .unwrap()
    };
    let store = Store::open(&project.db_path()).unwrap();

    // Two rejections, then success: the run completes on the third attempt.
    std::env::set_var("FAKE_REJECTIONS", "2");
    let (launch, _) = scheduler::create_launch(&project, &store, &exp("retry", 4), None, "").unwrap();
    let events = Mutex::new(vec![]);
    scheduler::execute(&project, &store, launch, &RunOptions { skip_judge: true, ..Default::default() }, Arc::new(AtomicBool::new(false)), &|e| events.lock().unwrap().push(e)).unwrap();
    let run = &store.runs(launch).unwrap()[0];
    assert_eq!(run.status, "done", "{}", run.error);
    assert_eq!(run.output_text.trim(), "ok on attempt 3");
    let kinds: Vec<&str> = run.events.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    // Each rejection leaves the runner's own event, the scheduler's record, and the retry.
    assert_eq!(kinds, vec!["rate_limit", "rate_limit", "retry", "rate_limit", "rate_limit", "retry"], "{:?}", run.events);
    assert_eq!(store.metrics_for_run(run.id).unwrap()["retries"], 2.0);
    let waits: Vec<(u32, u64)> = events.lock().unwrap().iter().filter_map(|e| if let Event::RateLimited { attempt, wait_s, .. } = e { Some((*attempt, *wait_s)) } else { None }).collect();
    assert_eq!(waits.iter().map(|w| w.0).collect::<Vec<_>>(), vec![1, 2], "the UI and CLI are told about each wait");
    // The report surfaces it instead of hiding it in timing.
    let rep = wrangler_core::report::build(&store, launch, None).unwrap();
    assert_eq!((rep.events.rate_limited_runs, rep.events.retries), (1, 2));
    assert!(rep.warnings.iter().any(|w| w.contains("rate limit")));
    // The workspace was reset between attempts: only the final attempt's files are in the diff.
    let diff = std::fs::read_to_string(std::path::Path::new(&run.dir).join("diff.patch")).unwrap();
    assert!(diff.contains("leftover-from-attempt-3") && !diff.contains("leftover-from-attempt-1"));

    // More rejections than retries: the run is recorded as failed, with the reason.
    std::env::set_var("FAKE_REJECTIONS", "99");
    let (launch, _) = scheduler::create_launch(&project, &store, &exp("exhausted", 1), None, "").unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions { skip_judge: true, ..Default::default() }, Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    let run = &store.runs(launch).unwrap()[0];
    assert_eq!(run.status, "failed");
    assert!(run.error.contains("usage limit"), "{}", run.error);
    assert_eq!(run.events.as_array().unwrap().iter().filter(|e| e["kind"] == "retry").count(), 1);
    // A later resume with retry_failed picks it up again.
    std::env::set_var("FAKE_REJECTIONS", "0");
    std::fs::remove_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    scheduler::execute(&project, &store, launch, &RunOptions { skip_judge: true, retry_failed: true, ..Default::default() }, Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
    assert_eq!(store.runs(launch).unwrap()[0].status, "done");
}

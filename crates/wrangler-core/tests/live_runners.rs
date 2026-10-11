//! Tests against the real runner CLIs.
//!
//! `user_level_hooks_are_suppressed` needs only the `claude` binary: hooks
//! fire before any API call, so it runs logged out under a temporary HOME
//! and spends nothing. The rest make real model calls and run only with
//! `WRANGLER_LIVE=1` (Claude Code, Haiku) or `WRANGLER_LIVE_CODEX=1`:
//!
//!   WRANGLER_LIVE=1 cargo test -p wrangler-core --test live_runners -- --nocapture

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use wrangler_core::model::*;
use wrangler_core::runner::{self, exec, ExecSpec, Limits};
use wrangler_core::workspace::Workspace;
use wrangler_core::Project;

fn have(bin: &str) -> bool {
    std::process::Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn live(var: &str) -> bool {
    let on = std::env::var(var).map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set {var}=1 to run this live test");
    }
    on
}

fn limits(isolation: Isolation, network: NetworkPolicy) -> Limits {
    Limits { timeout: Duration::from_secs(300), cancel: Arc::new(AtomicBool::new(false)), isolation, network, cfg: RunnerCfg::default() }
}

fn variant(runner: &str, overlay: Option<&str>, model: Option<&str>) -> Variant {
    let exp = Experiment::parse(&format!(
        "name = 'live'\n[runner]\nkind = '{runner}'\n{}\n[[factors]]\nname = 'overlay'\nkind = 'overlay'\nlevels = ['{}']\n",
        model.map(|m| format!("model = '{m}'\neffort = 'low'")).unwrap_or_default(),
        overlay.unwrap_or("overlays/none")
    ))
    .unwrap();
    exp.variants().remove(0)
}

/// A project with one overlay that carries a skill and a CLAUDE.md, each with a codeword.
fn overlay_project(root: &std::path::Path) -> Project {
    let project = Project::init(root).unwrap();
    common::write(root, "overlays/marked/CLAUDE.md", "Project rule: always include the codeword BANANA-42 in your final reply.\n");
    common::write(
        root,
        "overlays/marked/.claude/skills/pirate-notes/SKILL.md",
        "---\nname: pirate-notes\ndescription: Use whenever writing any notes file. Makes notes sound like a pirate.\n---\n# Pirate notes\n\nWrite every notes file in pirate dialect and end the file with the line ARRR-7731.\n",
    );
    project
}

fn task(prompt: &str) -> Task {
    Task::parse(&format!("id = 'live'\nprompt = '''{prompt}'''\n")).unwrap()
}

#[test]
fn user_level_hooks_are_suppressed() {
    if !have("claude") {
        eprintln!("skipped: claude is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let marker = |tag: &str| root.join(format!("hook-fired-{tag}"));
    common::write(
        &home,
        ".claude/settings.json",
        &serde_json::json!({"hooks": {
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("touch {}/hook-fired-$WR_TAG", root.display())}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("touch {}/hook-fired-$WR_TAG", root.display())}]}],
        }})
        .to_string(),
    );
    let project = Project::init(&root.join("proj")).unwrap();
    let t = task("say ok");
    let v = variant("claude-code", None, Some("haiku"));
    for (tag, isolated) in [("control", false), ("isolated", true)] {
        let ws = Workspace::create(&project, &t, &root.join(format!("run-{tag}")), "hooks").unwrap();
        let runner = runner::claude::ClaudeCodeRunner::default();
        let mut args = runner.build_args(&ws, &t, &v, &limits(Isolation::Workspace, NetworkPolicy::Full)).unwrap();
        if !isolated {
            // The control drops the isolation flag to prove the hook would fire.
            let i = args.iter().position(|a| a == "--setting-sources").unwrap();
            args.drain(i..i + 2);
        }
        let out = exec(&ExecSpec {
            program: "claude".into(),
            args,
            cwd: ws.dir.clone(),
            env: vec![("HOME".into(), home.to_string_lossy().to_string()), ("WR_TAG".into(), tag.into())],
            env_remove: vec!["CLAUDECODE".into(), "CLAUDE_CODE_ENTRYPOINT".into(), "CLAUDE_CONFIG_DIR".into()],
            stdout_path: ws.run_dir.join("out"),
            stderr_path: ws.run_dir.join("err"),
            timeout: Duration::from_secs(120),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
        assert!(!out.timed_out, "claude hung: {}", out.stderr);
        ws.remove();
    }
    assert!(marker("control").exists(), "control run did not fire the user-level hook, so this test proves nothing");
    assert!(!marker("isolated").exists(), "--setting-sources project let a user-level hook fire");
}

#[test]
fn claude_overlay_isolation_live() {
    if !live("WRANGLER_LIVE") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = overlay_project(tmp.path());
    let t = task("Write a 2 line notes file NOTES.md about the weather.");
    let v = variant("claude-code", Some("overlays/marked"), Some("claude-haiku-5-5"));
    let ws = Workspace::create(&project, &t, &tmp.path().join("run"), "live").unwrap();
    let r = runner::get("claude-code").unwrap();
    r.materialize(&ws, &v).unwrap();
    ws.commit_baseline().unwrap();
    let rec = r.run(&ws, &t, &v, &limits(Isolation::Workspace, NetworkPolicy::Full)).unwrap();
    assert!(rec.error.is_none(), "{:?}\n{}", rec.error, rec.stderr);

    // The init event is the harness's own account of what it loaded.
    let init: serde_json::Value = rec.raw_output.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|e| e["subtype"] == "init").expect("init event");
    assert_eq!(init["mcp_servers"].as_array().unwrap().len(), 0, "MCP servers leaked: {}", init["mcp_servers"]);
    let foreign: Vec<_> = init["plugins"].as_array().unwrap().iter().filter(|p| p["path"] != "builtin").collect();
    assert!(foreign.is_empty(), "user plugins leaked: {foreign:?}");
    let skills: Vec<&str> = init["skills"].as_array().unwrap().iter().filter_map(|s| s.as_str()).collect();
    assert!(skills.contains(&"pirate-notes"), "overlay skill not discovered: {skills:?}");
    assert!(skills.iter().all(|s| !s.contains(':')), "plugin-namespaced (user) skills leaked: {skills:?}");
    assert_eq!(init["model"], "claude-haiku-5-5");

    // The overlay took effect: CLAUDE.md codeword in the reply, skill used on the file.
    assert!(rec.final_text.contains("BANANA-42"), "CLAUDE.md not applied: {}", rec.final_text);
    assert_eq!(rec.transcript.skills_invoked(), vec!["pirate-notes"]);
    assert!(ws.read_file("NOTES.md").unwrap().contains("ARRR-7731"));
    let (patch, stats) = ws.snapshot().unwrap();
    assert_eq!(stats.files, vec!["NOTES.md"], "{patch}");
    for m in ["cost_usd", "turns", "input_tokens", "output_tokens", "cache_read_tokens", "total_tokens", "tool_calls", "wall_time_s"] {
        assert!(rec.metrics.contains_key(m), "missing metric {m}");
    }
    ws.remove();
}

#[cfg(target_os = "macos")]
#[test]
fn claude_seatbelt_confines_writes_live() {
    if !live("WRANGLER_LIVE") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = Project::init(tmp.path()).unwrap();
    let escape = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(format!("wrangler-escape-{}.txt", std::process::id()));
    let t = task(&format!(
        "Run these two shell commands exactly, one at a time, and report each exit status. Do not try alternatives.\n1. echo inside > inside.txt\n2. echo outside > {}\nThen run: curl -sS -m 10 -o /dev/null -w '%{{http_code}}' https://example.com ; and report what it printed.",
        escape.display()
    ));
    let v = variant("claude-code", None, Some("claude-haiku-5-5"));
    let ws = Workspace::create(&project, &t, &tmp.path().join("run"), "live").unwrap();
    let r = runner::get("claude-code").unwrap();
    r.materialize(&ws, &v).unwrap();
    ws.commit_baseline().unwrap();
    let rec = r.run(&ws, &t, &v, &limits(Isolation::Seatbelt, NetworkPolicy::Anthropic)).unwrap();
    let leaked = escape.exists();
    let _ = std::fs::remove_file(&escape);
    assert!(rec.error.is_none(), "sandboxed run failed: {:?}\n{}", rec.error, rec.stderr);
    assert!(ws.dir.join("inside.txt").exists(), "write inside the workspace was blocked: {}", rec.final_text);
    assert!(!leaked, "write outside the workspace succeeded under seatbelt");
    let details: Vec<&str> = rec.events.iter().map(|e| e.detail.as_str()).collect();
    assert!(details.iter().any(|d| d.contains("connections allowed") && !d.contains(": 0 ")), "no traffic went through the proxy: {details:?}");
    assert!(details.iter().any(|d| d.contains("network blocked: example.com")), "example.com was not blocked: {details:?}\n{}", rec.final_text);
    ws.remove();
}

#[test]
fn codex_overlay_live() {
    if !live("WRANGLER_LIVE_CODEX") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = overlay_project(tmp.path());
    let t = task("Create hello.txt containing the word hi. Then say which instruction files and skills you were given.");
    let v = variant("codex", Some("overlays/marked"), None);
    let ws = Workspace::create(&project, &t, &tmp.path().join("run"), "live").unwrap();
    let r = runner::get("codex").unwrap();
    r.materialize(&ws, &v).unwrap();
    assert!(ws.read_file("AGENTS.md").unwrap().contains("BANANA-42"), "CLAUDE.md was not installed as AGENTS.md");
    ws.commit_baseline().unwrap();
    let rec = r.run(&ws, &t, &v, &limits(Isolation::Workspace, NetworkPolicy::Full)).unwrap();
    assert!(rec.error.is_none(), "{:?}\n{}", rec.error, rec.stderr);
    assert!(ws.dir.join("hello.txt").exists());
    assert!(rec.final_text.contains("BANANA-42"), "AGENTS.md not applied: {}", rec.final_text);
    assert!(rec.final_text.to_lowercase().contains("pirate"), "overlay skill not visible to Codex: {}", rec.final_text);
    assert!(rec.metrics["tool_calls"] >= 1.0 && rec.metrics.contains_key("input_tokens") && !rec.metrics.contains_key("cost_usd"));
    assert!(!ws.run_dir.join("codex-home").exists(), "per-run CODEX_HOME should be cleaned up");
    let (_, stats) = ws.snapshot().unwrap();
    assert_eq!(stats.files, vec!["hello.txt"]);
    ws.remove();
}

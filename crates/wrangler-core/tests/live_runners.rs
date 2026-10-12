//! Tests against the real runner CLIs.
//!
//! `overlay_isolation_hides_user_level_config` needs only the `claude`
//! binary and runs on every `cargo test`: it works logged out under a
//! temporary HOME and spends nothing. The rest make real model calls and run only with
//! `WRANGLER_LIVE=1` (Claude Code, Haiku) or `WRANGLER_LIVE_CODEX=1`:
//!
//!   WRANGLER_LIVE=1 cargo test -p wrangler-core --test live_runners -- --nocapture

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use wrangler_core::model::*;
use wrangler_core::runner::{self, exec, AgentRunner, ExecSpec, Limits};
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

/// The overlay isolation claim from the design's spike, checked on every
/// test run. Claude Code reports what it loaded (the `init` event) and fires
/// hooks before any API call, so this runs logged out under a temporary HOME
/// and spends nothing. The control run drops the isolation flags to prove
/// each planted user-level item would otherwise be picked up.
#[test]
fn overlay_isolation_hides_user_level_config() {
    if !have("claude") {
        eprintln!("skipped: claude is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let marker = |tag: &str| root.join(format!("hook-fired-{tag}"));
    // User-level config: hooks, a skill, and an MCP server.
    common::write(
        &home,
        ".claude/settings.json",
        &serde_json::json!({"hooks": {
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("touch {}/hook-fired-$WR_TAG", root.display())}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("touch {}/hook-fired-$WR_TAG", root.display())}]}],
        }})
        .to_string(),
    );
    common::write(&home, ".claude/skills/user-only-skill/SKILL.md", "---\nname: user-only-skill\ndescription: A user-level skill.\n---\nbody\n");
    common::write(&home, ".claude.json", r#"{"mcpServers":{"user-mcp":{"command":"/bin/sleep","args":["600"]}}}"#);
    let project = overlay_project(&root.join("proj"));
    let t = task("say ok");
    let v = variant("claude-code", Some("overlays/marked"), Some("haiku"));
    let mut seen: Vec<(Vec<String>, Vec<String>)> = vec![];
    for (tag, isolated) in [("control", false), ("isolated", true)] {
        let ws = Workspace::create(&project, &t, &root.join(format!("run-{tag}")), "iso").unwrap();
        let runner = runner::claude::ClaudeCodeRunner::default();
        runner.materialize(&ws, &v).unwrap();
        let mut args = runner.build_args(&ws, &t, &v, &limits(Isolation::Workspace, NetworkPolicy::Full)).unwrap();
        if !isolated {
            let i = args.iter().position(|a| a == "--setting-sources").unwrap();
            args.drain(i..i + 2);
            args.retain(|a| a != "--strict-mcp-config");
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
        let init: serde_json::Value = out.stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|e| e["subtype"] == "init").unwrap_or_else(|| panic!("no init event in {tag} run: {}", out.stdout));
        let names = |v: &serde_json::Value, key: Option<&str>| -> Vec<String> {
            v.as_array().into_iter().flatten().filter_map(|x| match key { Some(k) => x[k].as_str(), None => x.as_str() }).map(|s| s.to_string()).collect()
        };
        seen.push((names(&init["skills"], None), names(&init["mcp_servers"], Some("name"))));
        ws.remove();
    }
    let (control, isolated) = (&seen[0], &seen[1]);
    // The control proves the test can see each leak.
    assert!(marker("control").exists(), "control run did not fire the user-level hook");
    assert!(control.0.iter().any(|s| s == "user-only-skill"), "control run did not load the user-level skill: {:?}", control.0);
    assert!(control.1.iter().any(|s| s == "user-mcp"), "control run did not load the user-level MCP server: {:?}", control.1);
    // Isolated: the overlay is in, everything user-level is out.
    assert!(isolated.0.iter().any(|s| s == "pirate-notes"), "overlay skill was not discovered: {:?}", isolated.0);
    assert!(!marker("isolated").exists(), "--setting-sources project let a user-level hook fire");
    assert!(!isolated.0.iter().any(|s| s == "user-only-skill"), "user-level skill leaked: {:?}", isolated.0);
    assert!(isolated.1.is_empty(), "MCP servers leaked: {:?}", isolated.1);
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

/// Hook executions are visible in the stream: an overlay that installs a
/// `PreToolUse` hook on Bash yields `hook.PreToolUse` and a hook event that
/// carries the matcher, exit code and output.
#[test]
fn claude_hooks_are_recorded_live() {
    if !live("WRANGLER_LIVE") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = Project::init(tmp.path()).unwrap();
    common::write(
        tmp.path(),
        "overlays/hooked/.claude/settings.json",
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo HOOK-SAW-BASH; exit 0"}]}]}}"#,
    );
    let t = task("Run the shell command `echo hi` once, then reply with the single word done.");
    let v = variant("claude-code", Some("overlays/hooked"), Some("claude-haiku-5-5"));
    let ws = Workspace::create(&project, &t, &tmp.path().join("run"), "live").unwrap();
    let r = runner::get("claude-code").unwrap();
    r.materialize(&ws, &v).unwrap();
    ws.commit_baseline().unwrap();
    let rec = r.run(&ws, &t, &v, &limits(Isolation::Workspace, NetworkPolicy::Full)).unwrap();
    assert!(rec.error.is_none(), "{:?}\n{}", rec.error, rec.stderr);
    assert!(rec.metrics.get("hook.PreToolUse").copied().unwrap_or(0.0) >= 1.0, "no PreToolUse hook recorded: {:?}", rec.metrics);
    let hook = rec.events.iter().find(|e| e.kind == "hook" && e.data["event"] == "PreToolUse").expect("a hook event");
    assert_eq!(hook.data["exit_code"], 0, "{hook:?}");
    assert_eq!(hook.data["matcher"], "Bash", "{hook:?}");
    assert!(hook.data["output"].as_str().unwrap_or("").contains("HOOK-SAW-BASH"), "{hook:?}");
    // The graders that read the process agree with the stream.
    let specs: Vec<DetSpec> = serde_json::from_value(serde_json::json!([
        {"kind": "hook_fired", "event": "PreToolUse"},
        {"kind": "sequence", "steps": [{"name": ["Bash", "shell"], "input": "echo hi"}]},
        {"kind": "tool_call", "name": "Bash", "min": 1},
    ]))
    .unwrap();
    let (_, diff) = ws.snapshot().unwrap();
    let grades = wrangler_core::graders::deterministic::grade_all(&specs, &ws, &rec.final_text, &diff, &rec.transcript, Some(&rec.events), 1);
    assert!(grades.iter().all(|g| g.passed == Some(true)), "{grades:#?}");
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

//! Isolation ladder, level 2: macOS seatbelt (`sandbox-exec`).
//!
//! File writes are confined to the workspace plus the paths the runner
//! itself needs (measured, see `claude_writable_paths`). With
//! `network = "anthropic"`, direct outbound traffic is denied and the agent
//! is pointed at a local allowlisting CONNECT proxy; seatbelt cannot filter
//! by hostname, so the proxy is what enforces the host list.

use crate::model::{Isolation, NetworkPolicy};
use crate::runner::{ExecSpec, Limits, RunEvent};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Hosts reachable under `network = "anthropic"` (suffix match).
pub const ANTHROPIC_HOSTS: &[&str] = &["anthropic.com", "claude.ai", "claude.com"];

pub struct Sandbox {
    profile_path: PathBuf,
    proxy: Option<Proxy>,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Paths outside the workspace that Claude Code writes during a `-p` run
/// with `--no-session-persistence`. Measured on Claude Code 2.1.296 by
/// running under a workspace-only write profile with `--debug-file` and
/// collecting every EPERM; `docs/isolation.md` has the method and raw list.
pub fn claude_writable_paths() -> Vec<SbPath> {
    let h = home();
    let uid = unsafe { libc::getuid() };
    vec![
        // shell-snapshots/, session-env/, sessions/ and per-project memory.
        // Without shell-snapshots the Bash tool cannot start at all.
        SbPath::Subpath(h.join(".claude")),
        // Global state file with its `.tmp.<pid>.<hex>` and `.lock` siblings.
        SbPath::Prefix(h.join(".claude.json")),
        // Version lock files.
        SbPath::Subpath(h.join(".local/state/claude")),
        // Per-user scratch: the Bash tool's scratchpad and fswatch probes.
        SbPath::Subpath(PathBuf::from(format!("/private/tmp/claude-{uid}"))),
    ]
}

#[derive(Debug, Clone)]
pub enum SbPath {
    Subpath(PathBuf),
    /// Any path starting with this string (for `name`, `name.lock`, `name.tmp.123`).
    Prefix(PathBuf),
}

fn sb_quote(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build the seatbelt profile text.
pub fn profile(workspace: &Path, extra: &[SbPath], network: NetworkPolicy, proxy_port: Option<u16>) -> String {
    let mut s = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n");
    s.push_str(&format!("  (subpath \"{}\")\n", sb_quote(&canon(workspace))));
    for p in extra {
        match p {
            SbPath::Subpath(p) => s.push_str(&format!("  (subpath \"{}\")\n", sb_quote(&canon(p)))),
            SbPath::Prefix(p) => s.push_str(&format!("  (regex #\"^{}\")\n", regex::escape(&p.to_string_lossy()).replace('"', "\\\""))),
        }
    }
    // Device nodes and the per-process temp dirs every Unix tool assumes.
    s.push_str("  (literal \"/dev/null\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\")\n  (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/\")\n)\n");
    if network == NetworkPolicy::Anthropic {
        s.push_str("(deny network-outbound)\n");
        if let Some(port) = proxy_port {
            s.push_str(&format!("(allow network-outbound (remote ip \"localhost:{port}\"))\n"));
        }
        // Local IPC (keychain, system services) is unix-socket and mach traffic, not internet.
        s.push_str("(allow network-outbound (remote unix-socket))\n");
    }
    s
}

impl Sandbox {
    /// The sandbox for a Claude Code run, or `None` below the seatbelt level.
    pub fn for_claude(ws: &Workspace, limits: &Limits) -> Result<Option<Sandbox>> {
        if limits.isolation != Isolation::Seatbelt {
            return Ok(None);
        }
        let proxy = match limits.network {
            NetworkPolicy::Anthropic => Some(Proxy::start(ANTHROPIC_HOSTS.iter().map(|s| s.to_string()).collect())?),
            NetworkPolicy::Full => None,
        };
        let text = profile(&ws.dir, &claude_writable_paths(), limits.network, proxy.as_ref().map(|p| p.port));
        let profile_path = ws.run_dir.join("sandbox.sb");
        std::fs::write(&profile_path, text)?;
        Ok(Some(Sandbox { profile_path, proxy }))
    }

    /// Rewrite the command to run under `sandbox-exec`.
    pub fn apply(&self, spec: &mut ExecSpec) {
        let mut args = vec!["-f".to_string(), self.profile_path.to_string_lossy().to_string(), std::mem::take(&mut spec.program)];
        args.append(&mut spec.args);
        spec.program = "/usr/bin/sandbox-exec".into();
        spec.args = args;
        if let Some(p) = &self.proxy {
            let url = format!("http://127.0.0.1:{}", p.port);
            for k in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
                spec.env.push((k.into(), url.clone()));
            }
            spec.env.push(("NO_PROXY".into(), String::new()));
        }
    }

    /// Stop the proxy and report what it blocked.
    pub fn finish(self) -> Vec<RunEvent> {
        let mut events = vec![RunEvent { kind: "sandbox".into(), at_ms: 0, detail: "ran under seatbelt".into() }];
        if let Some(p) = self.proxy {
            let (allowed, blocked) = p.stop();
            events.push(RunEvent { kind: "sandbox".into(), at_ms: 0, detail: format!("network proxy: {allowed} connections allowed") });
            for host in blocked {
                events.push(RunEvent { kind: "sandbox".into(), at_ms: 0, detail: format!("network blocked: {host}") });
            }
        }
        events
    }
}

// ----------------------------------------------------------------- docker

/// Isolation ladder, level 3: run the runner CLI inside a container. Only
/// the workspace (and any extra paths the runner needs) is mounted, at the
/// same path as on the host so transcripts and diffs line up. Optional: no
/// other isolation level touches Docker.
pub struct DockerRun {
    name: String,
}

pub const DOCKER_DEFAULT_ENV: &[&str] = &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "OPENAI_API_KEY"];

/// The `docker run` argument list that wraps `program args`.
pub fn docker_args(name: &str, image: &str, workspace: &Path, mounts: &[(PathBuf, bool)], env: &[(String, String)], pass_env: &[String], extra: &[String], program: &str, args: &[String]) -> Vec<String> {
    let ws = canon(workspace).to_string_lossy().to_string();
    let mut a: Vec<String> = vec!["run".into(), "--rm".into(), "-i".into(), "--name".into(), name.into(), "-v".into(), format!("{ws}:{ws}"), "-w".into(), ws];
    for (path, read_only) in mounts {
        let p = canon(path).to_string_lossy().to_string();
        a.extend(["-v".into(), format!("{p}:{p}{}", if *read_only { ":ro" } else { "" })]);
    }
    for (k, v) in env {
        a.extend(["-e".into(), format!("{k}={v}")]);
    }
    // By name only: the value comes from the docker client's environment
    // and never appears in the recorded command line.
    for k in pass_env {
        if std::env::var_os(k).is_some() {
            a.extend(["-e".into(), k.clone()]);
        }
    }
    a.extend(extra.iter().cloned());
    a.push(image.into());
    a.push(program.into());
    a.extend(args.iter().cloned());
    a
}

/// Is a Docker daemon answering? Time-boxed: a daemon that is starting or
/// stuck makes `docker info` hang, and validation must not hang with it.
pub fn docker_available() -> bool {
    let child = std::process::Command::new("docker").arg("info").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    let Ok(mut child) = child else { return false };
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

impl DockerRun {
    /// Rewrite the command to run in a container, or `None` below the docker level.
    /// `mounts` are extra host paths (path, read_only) the runner needs inside.
    pub fn wrap(ws: &Workspace, limits: &Limits, spec: &mut ExecSpec, mounts: &[(PathBuf, bool)]) -> Result<Option<DockerRun>> {
        if limits.isolation != Isolation::Docker {
            return Ok(None);
        }
        let image = limits.cfg.docker_image.clone().context("isolation = \"docker\" needs [runner] docker_image")?;
        let name = format!("wrangler-{}", uuid::Uuid::new_v4().simple());
        let pass: Vec<String> = if limits.cfg.docker_env.is_empty() { DOCKER_DEFAULT_ENV.iter().map(|s| s.to_string()).collect() } else { limits.cfg.docker_env.clone() };
        let mut all_mounts = mounts.to_vec();
        for p in &limits.cfg.plugin_dirs {
            all_mounts.push((ws.project.abs(p), true));
        }
        // Run as the host user, not root: files in the mounted workspace keep
        // their owner, and Claude Code refuses bypassPermissions as root.
        // HOME points somewhere any uid can write.
        let mut extra: Vec<String> = vec![];
        if !limits.cfg.docker_args.iter().any(|a| a == "--user" || a == "-u" || a.starts_with("--user=")) {
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            extra.extend(["--user".into(), format!("{uid}:{gid}"), "-e".into(), "HOME=/tmp".into()]);
        }
        extra.extend(limits.cfg.docker_args.iter().cloned());
        spec.args = docker_args(&name, &image, &ws.dir, &all_mounts, &spec.env, &pass, &extra, &spec.program, &spec.args);
        spec.program = "docker".into();
        spec.env.clear();
        Ok(Some(DockerRun { name }))
    }

    /// Make sure the container is gone (a killed client can leave it running).
    pub fn finish(self) -> Vec<RunEvent> {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.name]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
        vec![RunEvent { kind: "sandbox".into(), at_ms: 0, detail: format!("ran in docker container {}", self.name) }]
    }
}

// ------------------------------------------------------------------ proxy

/// Minimal HTTP CONNECT proxy that only tunnels to allowlisted hosts.
pub struct Proxy {
    pub port: u16,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<(u64, Vec<String>)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    let h = host.trim_end_matches('.').to_lowercase();
    allow.iter().any(|a| h == *a || h.ends_with(&format!(".{a}")))
}

impl Proxy {
    pub fn start(allow: Vec<String>) -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding proxy port")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new((0u64, Vec::<String>::new())));
        let (stop2, stats2) = (stop.clone(), stats.clone());
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let (allow, stats, stop) = (allow.clone(), stats2.clone(), stop2.clone());
                        std::thread::spawn(move || {
                            let _ = handle(conn, &allow, &stats, &stop);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        Ok(Proxy { port, stop, stats, thread: Some(thread) })
    }

    pub fn stop(mut self) -> (u64, Vec<String>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let s = self.stats.lock().unwrap();
        (s.0, s.1.clone())
    }
}

fn handle(mut conn: TcpStream, allow: &[String], stats: &Mutex<(u64, Vec<String>)>, stop: &Arc<AtomicBool>) -> std::io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 16_384 {
        if conn.read(&mut byte)? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut parts = text.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target).trim_start_matches('[').trim_end_matches(']').to_string();
    if method != "CONNECT" || !host_allowed(&host, allow) {
        let mut s = stats.lock().unwrap();
        let label = if method == "CONNECT" { host } else { format!("{method} {target}") };
        if !s.1.contains(&label) {
            s.1.push(label);
        }
        conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let upstream = match TcpStream::connect(target) {
        Ok(u) => u,
        Err(_) => {
            conn.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
    };
    stats.lock().unwrap().0 += 1;
    conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    let pump = |mut from: TcpStream, mut to: TcpStream, stop: Arc<AtomicBool>| {
        std::thread::spawn(move || {
            let _ = from.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; 16_384];
            loop {
                match from.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Both);
            let _ = from.shutdown(std::net::Shutdown::Both);
        })
    };
    let a = pump(conn.try_clone()?, upstream.try_clone()?, stop.clone());
    let b = pump(upstream, conn, stop.clone());
    let _ = a.join();
    let _ = b.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_suffixes_only() {
        let allow: Vec<String> = ANTHROPIC_HOSTS.iter().map(|s| s.to_string()).collect();
        assert!(host_allowed("api.anthropic.com", &allow));
        assert!(host_allowed("claude.ai", &allow));
        assert!(!host_allowed("evilanthropic.com", &allow));
        assert!(!host_allowed("anthropic.com.evil.net", &allow));
        assert!(!host_allowed("example.com", &allow));
    }

    #[test]
    fn proxy_blocks_and_records() {
        let p = Proxy::start(vec!["anthropic.com".into()]).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", p.port)).unwrap();
        c.write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 403"));
        let (allowed, blocked) = p.stop();
        assert_eq!(allowed, 0);
        assert_eq!(blocked, vec!["example.com"]);
    }

    #[test]
    fn docker_command_shape() {
        std::env::set_var("WRANGLER_TEST_SECRET", "s3cret");
        let a = docker_args("n1", "img:1", Path::new("/tmp"), &[(PathBuf::from("/usr"), true)], &[("K".into(), "v".into())], &["WRANGLER_TEST_SECRET".into(), "WRANGLER_UNSET_VAR".into()], &["--network".into(), "none".into()], "claude", &["-p".into(), "hi".into()]);
        let joined = a.join(" ");
        assert!(joined.starts_with("run --rm -i --name n1 -v "), "{joined}");
        assert!(joined.contains(":ro") && joined.contains("-e K=v") && joined.contains("-e WRANGLER_TEST_SECRET"));
        assert!(!joined.contains("s3cret") && !joined.contains("WRANGLER_UNSET_VAR"), "secrets pass by name only: {joined}");
        assert!(joined.ends_with("--network none img:1 claude -p hi"), "{joined}");
    }

    #[test]
    fn profile_confines_writes() {
        let text = profile(Path::new("/tmp"), &[SbPath::Prefix(PathBuf::from("/Users/x/.claude.json"))], NetworkPolicy::Anthropic, Some(4040));
        assert!(text.contains("(deny file-write*)"));
        assert!(text.contains("localhost:4040"));
        assert!(text.contains(r#"(regex #"^/Users/x/\.claude\.json")"#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_enforces_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = canon(tmp.path()).join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let outside = canon(tmp.path()).join("outside.txt");
        let prof = canon(tmp.path()).join("p.sb");
        std::fs::write(&prof, profile(&ws, &[], NetworkPolicy::Full, None)).unwrap();
        let run = |script: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec").arg("-f").arg(&prof).args(["/bin/sh", "-c", script]).current_dir(&ws).status().unwrap().success()
        };
        assert!(run("echo hi > inside.txt"));
        assert!(!run(&format!("echo hi > '{}' 2>/dev/null", outside.display())));
        assert!(ws.join("inside.txt").exists() && !outside.exists());
    }
}

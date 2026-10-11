//! The in-app terminal: a pseudo-terminal running Claude Code in the project
//! folder, told what Harness Wrangler is and how its files and commands
//! work, with the `wrangler` CLI on its PATH.

use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

const BACKLOG_BYTES: usize = 400_000;

pub struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Recent output, replayed when the panel is reopened.
    backlog: Arc<Mutex<Vec<u8>>>,
    pub root: PathBuf,
}

#[derive(Default)]
pub struct Agent {
    pub session: Mutex<Option<Session>>,
}

/// What the agent is told about this tool. The file-format reference is the
/// same document that ships in docs/.
pub fn briefing(root: &Path) -> String {
    format!(
        r#"You are running inside the Harness Wrangler desktop app, in a terminal panel beside its UI. The person is using Harness Wrangler in this project: {root}

Harness Wrangler runs controlled experiments over coding-agent harness patterns (skills, CLAUDE.md files, system prompt appends, effort levels, models). It runs each task under each variant several times, grades the results (deterministic graders, an LLM judge, blind human review), and reports effect sizes with confidence intervals, then concrete advice.

How you help here:
- Draft and edit the project's files: tasks under tasks/, fixtures under fixtures/, rubrics under rubrics/, experiments under experiments/, suites under suites/. The app picks up file changes on its own.
- Answer questions about results by reading them with the CLI, which is on PATH as `wrangler`. Every command takes --json.
- After writing an experiment, check it with `wrangler validate <name>`.
- Do not start an experiment run (`wrangler run`, `wrangler suite run`) unless the person asks: runs cost real usage. Say what it would cost first (`wrangler validate` prints the run count).
- Good tasks are specific and checkable: a concrete prompt, a fixture with realistic source material, an output_file, 3 to 6 expectations a judge can verify by quoting the result, and deterministic graders where a rule can be checked mechanically. Tasks in one experiment should differ from each other.

Commands:
  wrangler list experiments|tasks|overlays|launches|suites
  wrangler validate <experiment>            check a definition, show its cells and run count
  wrangler run <experiment> [--reps N] [--budget USD] [--no-judge]
  wrangler resume <launch> [--retry-failed] [--budget USD]
  wrangler status <launch>                  progress and cost per cell
  wrangler report <launch|experiment> [--metric m] [--json]
  wrangler runs <launch> [--status s] [--task t] [--variant v]
  wrangler show <run> [--output|--diff|--transcript]
  wrangler advice <launch|experiment> [--json]
  wrangler annotations <launch> [--json]    human review comments and reactions, by variant
  wrangler ablate <skill-or-CLAUDE.md> [--description-only] [--depth N]
  wrangler suite create|run|status|diff
A launch is one execution of an experiment; <launch> is its number, or the experiment name for its latest launch.

File format reference:

{formats}"#,
        root = root.display(),
        formats = include_str!("../../../../docs/formats.md")
    )
}

fn emit_chunk(app: &AppHandle, bytes: &[u8]) {
    let _ = app.emit("agent-output", base64::engine::general_purpose::STANDARD.encode(bytes));
}

impl Agent {
    /// Start the agent in `root`, or reattach to the one already running
    /// there. Returns the output so far, base64 encoded.
    pub fn start(&self, app: &AppHandle, root: &Path, cols: u16, rows: u16) -> Result<String, String> {
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            let alive = s.child.try_wait().map(|st| st.is_none()).unwrap_or(false);
            if alive && s.root == root {
                let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                return Ok(base64::engine::general_purpose::STANDARD.encode(&*s.backlog.lock().unwrap()));
            }
            let _ = s.child.kill();
        }
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        // Bundled CLI first on PATH, so `wrangler` always matches this app.
        let mut path_dirs: Vec<PathBuf> = vec![];
        if let Ok(res) = app.path().resource_dir() {
            path_dirs.push(res.join("bin"));
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
            path_dirs.push(dir);
        }
        let path = std::env::var("PATH").unwrap_or_default();
        let full_path = path_dirs.iter().filter(|d| d.join("wrangler").exists()).map(|d| d.to_string_lossy().to_string()).chain(std::iter::once(path.clone())).collect::<Vec<_>>().join(":");
        let has_claude = full_path.split(':').any(|d| Path::new(d).join("claude").is_file());
        let mut cmd = if has_claude {
            let mut c = CommandBuilder::new("claude");
            c.arg("--append-system-prompt");
            c.arg(briefing(root));
            c
        } else {
            // No agent CLI: still give a shell, and say why.
            let mut c = CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()));
            c.arg("-l");
            c
        };
        cmd.cwd(root);
        cmd.env("PATH", &full_path);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for k in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT"] {
            cmd.env_remove(k);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("could not start the agent: {e}"))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let backlog = Arc::new(Mutex::new(Vec::<u8>::new()));
        if !has_claude {
            let note = b"Claude Code was not found on PATH, so this is a plain shell. Install it to get the agent here.\r\n";
            backlog.lock().unwrap().extend_from_slice(note);
        }
        let (app2, backlog2) = (app.clone(), backlog.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        {
                            let mut b = backlog2.lock().unwrap();
                            b.extend_from_slice(&buf[..n]);
                            if b.len() > BACKLOG_BYTES {
                                let cut = b.len() - BACKLOG_BYTES;
                                b.drain(..cut);
                            }
                        }
                        emit_chunk(&app2, &buf[..n]);
                    }
                }
            }
            let _ = app2.emit("agent-exit", ());
        });
        let initial = base64::engine::general_purpose::STANDARD.encode(&*backlog.lock().unwrap());
        *guard = Some(Session { master: pair.master, writer, child, backlog, root: root.to_path_buf() });
        Ok(initial)
    }

    pub fn input(&self, data: &str) -> Result<(), String> {
        match self.session.lock().unwrap().as_mut() {
            Some(s) => s.writer.write_all(data.as_bytes()).and_then(|_| s.writer.flush()).map_err(|e| e.to_string()),
            None => Err("the agent is not running".into()),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(s) = self.session.lock().unwrap().as_mut() {
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    pub fn stop(&self) {
        if let Some(mut s) = self.session.lock().unwrap().take() {
            let _ = s.child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefing_carries_the_formats_and_commands() {
        let b = briefing(Path::new("/tmp/proj"));
        assert!(b.contains("/tmp/proj") && b.contains("wrangler validate") && b.contains("## Experiment") && b.contains("Deterministic graders"));
        assert!(b.contains("Do not start an experiment run"));
        assert!(b.len() < 60_000, "must fit comfortably in a command-line argument");
    }
}

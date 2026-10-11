//! Desktop shell. The backend is wrangler-core called in-process: every UI
//! action goes through `api`, the same command surface `wrangler serve`
//! exposes over HTTP. The shell itself only owns which project is open.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, State};
use wrangler_core::api::Api;
use wrangler_core::Project;

#[derive(Default)]
struct Shell {
    /// Project being opened in the background at startup, if any.
    opening: Mutex<Option<String>>,
    api: Mutex<Option<Arc<Api>>>,
    config_dir: Mutex<Option<PathBuf>>,
}

fn recent_file(shell: &Shell) -> Option<PathBuf> {
    shell.config_dir.lock().unwrap().clone().map(|d| d.join("recent-projects.json"))
}

fn read_recent(shell: &Shell) -> Vec<String> {
    recent_file(shell)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

fn remember(shell: &Shell, root: &Path) {
    let me = root.to_string_lossy().to_string();
    let mut list = read_recent(shell);
    list.retain(|p| p != &me);
    list.insert(0, me);
    list.truncate(8);
    if let Some(f) = recent_file(shell) {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, serde_json::to_string_pretty(&list).unwrap_or_default());
    }
}

fn open_project(shell: &Shell, project: Project) -> Result<String, String> {
    let root = project.root.clone();
    let api = Api::new(project).map_err(|e| format!("{e:#}"))?;
    *shell.api.lock().unwrap() = Some(api);
    remember(shell, &root);
    Ok(root.to_string_lossy().to_string())
}

fn open_at(shell: &Shell, path: &str) -> Result<String, String> {
    // Accept the project folder itself or anything inside it.
    let project = Project::discover(Path::new(path)).map_err(|_| format!("{path} is not a Harness Wrangler project (no wrangler.toml or experiments/ folder). Use \"Create project in a folder\" to start one there."))?;
    open_project(shell, project)
}

fn init_at(shell: &Shell, path: &str) -> Result<String, String> {
    let project = Project::init(Path::new(path)).map_err(|e| format!("{e:#}"))?;
    open_project(shell, project)
}

#[tauri::command]
fn project_open(path: String, shell: State<Shell>, app: AppHandle) -> Result<String, String> {
    let root = open_at(&shell, &path)?;
    install_menu(&app);
    Ok(root)
}

#[tauri::command]
fn project_init(path: String, shell: State<Shell>, app: AppHandle) -> Result<String, String> {
    let root = init_at(&shell, &path)?;
    install_menu(&app);
    Ok(root)
}

/// File menu: the project actions, with the recent projects listed. Rebuilt
/// whenever the recent list changes.
fn install_menu(app: &AppHandle) {
    let build = || -> tauri::Result<()> {
        let menu = Menu::default(app)?;
        let recent = read_recent(&app.state::<Shell>());
        let recent_menu = Submenu::with_id(app, "project-recent", "Open Recent", !recent.is_empty())?;
        for (i, path) in recent.iter().enumerate() {
            recent_menu.append(&MenuItem::with_id(app, format!("project-recent-{i}"), path, true, None::<&str>)?)?;
        }
        for item in menu.items()? {
            if let Some(file) = item.as_submenu().filter(|m| m.text().map(|t| t == "File").unwrap_or(false)) {
                file.insert(&MenuItem::with_id(app, "project-open", "Open Project…", true, Some("CmdOrCtrl+O"))?, 0)?;
                file.insert(&MenuItem::with_id(app, "project-new", "New Project…", true, Some("CmdOrCtrl+Shift+N"))?, 1)?;
                file.insert(&recent_menu, 2)?;
                file.insert(&MenuItem::with_id(app, "project-close", "Close Project", true, None::<&str>)?, 3)?;
                file.insert(&PredefinedMenuItem::separator(app)?, 4)?;
            }
        }
        app.set_menu(menu)?;
        Ok(())
    };
    let _ = build();
}

#[tauri::command]
fn project_current(shell: State<Shell>) -> Option<String> {
    shell.api.lock().unwrap().as_ref().map(|a| a.project.root.to_string_lossy().to_string())
}

/// Path of the project still being opened at startup, if any.
#[tauri::command]
fn project_pending(shell: State<Shell>) -> Option<String> {
    shell.opening.lock().unwrap().clone()
}

#[tauri::command]
fn project_close(shell: State<Shell>) {
    *shell.api.lock().unwrap() = None;
}

#[tauri::command]
fn recent_projects(shell: State<Shell>) -> Vec<String> {
    read_recent(&shell)
}

/// The single entry point for everything the UI does with a project.
#[tauri::command]
async fn api(cmd: String, args: Value, shell: State<'_, Shell>) -> Result<Value, String> {
    let api = shell.api.lock().unwrap().clone().ok_or("no project is open")?;
    // Core calls block on SQLite, git and the filesystem; keep them off the UI thread.
    tauri::async_runtime::spawn_blocking(move || api.dispatch(&cmd, &args).map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}

/// Apps started from Finder get a bare PATH without the user's shell setup,
/// so `claude`, `codex` and `git` would not be found. Ask a login shell for
/// its PATH, then add the usual install locations.
///
/// The shell is non-interactive and time-boxed: an interactive shell with
/// no terminal can block forever in the user's rc files, which would hang
/// the app before its window opens.
fn adopt_login_shell_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut dirs: Vec<String> = vec![];
    let child = std::process::Command::new(&shell)
        .args(["-lc", "printf '__PATH__%s__END__' \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let finished = loop {
            match child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(25)),
                _ => break false,
            }
        };
        if finished {
            if let Ok(out) = child.wait_with_output() {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if let Some(path) = text.split("__PATH__").nth(1).and_then(|r| r.split("__END__").next()) {
                    dirs.extend(path.split(':').filter(|d| !d.is_empty()).map(|d| d.to_string()));
                }
            }
        } else {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    dirs.extend(std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()).map(|d| d.to_string()));
    // PATH set only in an interactive rc file is not visible above.
    let home = std::env::var("HOME").unwrap_or_default();
    for d in [format!("{home}/.local/bin"), format!("{home}/.claude/local"), "/opt/homebrew/bin".into(), "/usr/local/bin".into(), format!("{home}/.cargo/bin"), format!("{home}/.bun/bin"), format!("{home}/.npm-global/bin"), format!("{home}/.volta/bin")] {
        if Path::new(&d).is_dir() {
            dirs.push(d);
        }
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    std::env::set_var("PATH", dirs.join(":"));
}

pub fn run() {
    adopt_login_shell_path();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Shell::default())
        .setup(|app| {
            let shell = app.state::<Shell>();
            *shell.config_dir.lock().unwrap() = app.path().app_config_dir().ok();
            // Open straight into a project when one is given or was open last
            // time. Off the main thread: the first read of a folder under
            // Documents or Desktop makes macOS ask for permission, and that
            // prompt must not hold up the window.
            let candidates: Vec<String> = std::env::args().nth(1).into_iter().chain(std::env::var("WRANGLER_PROJECT").ok()).chain(read_recent(&shell).into_iter().take(1)).collect();
            if let Some(first) = candidates.first() {
                *shell.opening.lock().unwrap() = Some(first.clone());
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let shell = handle.state::<Shell>();
                    let opened = candidates.iter().any(|c| open_at(&shell, c).is_ok());
                    *shell.opening.lock().unwrap() = None;
                    install_menu(&handle);
                    let _ = handle.emit("project-changed", (!opened).then(|| "The last project could not be opened. Choose a project folder.".to_string()));
                });
            }
            install_menu(app.handle());
            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_string();
            if let Some(i) = id.strip_prefix("project-recent-").and_then(|n| n.parse::<usize>().ok()) {
                // Opening a recent project needs no dialog: do it here, then tell the window.
                let shell = app.state::<Shell>();
                if let Some(path) = read_recent(&shell).get(i).cloned() {
                    let result = open_at(&shell, &path);
                    install_menu(app);
                    let _ = app.emit("project-changed", result.err());
                }
            } else if id.starts_with("project-") {
                // Open, new and close run in the window, which owns the folder picker.
                let _ = app.emit("project-menu", id);
            }
        })
        .invoke_handler(tauri::generate_handler![project_open, project_init, project_current, project_pending, project_close, recent_projects, api])
        .run(tauri::generate_context!())
        .expect("error while running Harness Wrangler");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_lookup_cannot_hang_and_finds_the_usual_places() {
        let before = std::env::var("PATH").unwrap_or_default();
        // A "shell" that never returns must not block startup.
        let tmp = std::env::temp_dir().join(format!("wrangler-slow-shell-{}", std::process::id()));
        std::fs::write(&tmp, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let real_shell = std::env::var("SHELL").ok();
        std::env::set_var("SHELL", &tmp);
        std::env::set_var("PATH", "/usr/bin:/bin");
        let start = std::time::Instant::now();
        adopt_login_shell_path();
        assert!(start.elapsed() < std::time::Duration::from_secs(6), "startup blocked for {:?}", start.elapsed());
        let path = std::env::var("PATH").unwrap();
        assert!(path.contains("/usr/bin"));
        let home = std::env::var("HOME").unwrap();
        if Path::new(&format!("{home}/.local/bin")).is_dir() {
            assert!(path.contains(&format!("{home}/.local/bin")), "{path}");
        }
        match real_shell {
            Some(s) => std::env::set_var("SHELL", s),
            None => std::env::remove_var("SHELL"),
        }
        std::env::set_var("PATH", before);
        let _ = std::fs::remove_file(&tmp);
    }

    fn shell(config: &Path) -> Shell {
        let s = Shell::default();
        *s.config_dir.lock().unwrap() = Some(config.to_path_buf());
        s
    }

    #[test]
    fn create_open_and_recent_projects() {
        let tmp = std::env::temp_dir().join(format!("wrangler-shell-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (a, b, plain) = (tmp.join("a"), tmp.join("b"), tmp.join("plain"));
        std::fs::create_dir_all(&plain).unwrap();
        let sh = shell(&tmp.join("config"));

        // Opening a folder that is not a project explains what to do instead.
        let err = open_at(&sh, plain.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a Harness Wrangler project") && err.contains("Create project"), "{err}");
        assert!(sh.api.lock().unwrap().is_none());

        // Creating one lays out the folders and opens it.
        let root_a = init_at(&sh, a.to_str().unwrap()).unwrap();
        for d in ["experiments", "tasks", "overlays/none", "fixtures", "rubrics", "suites"] {
            assert!(Path::new(&root_a).join(d).is_dir(), "missing {d}");
        }
        let api = sh.api.lock().unwrap().clone().unwrap();
        assert_eq!(api.dispatch("experiments_list", &json!({})).unwrap(), json!([]));
        assert!(api.dispatch("project_info", &json!({})).unwrap()["root"].as_str().unwrap().ends_with("/a"));
        drop(api);

        // A second project, then reopening the first from a folder inside it.
        let root_b = init_at(&sh, b.to_str().unwrap()).unwrap();
        let reopened = open_at(&sh, a.join("experiments").to_str().unwrap()).unwrap();
        assert_eq!(reopened, root_a);
        // Most recent first, no duplicates, and it survives a restart of the shell.
        assert_eq!(read_recent(&sh), vec![root_a.clone(), root_b.clone()]);
        assert_eq!(read_recent(&shell(&tmp.join("config"))), vec![root_a, root_b.clone()]);
        // Folders that have since been deleted drop out of the list.
        *sh.api.lock().unwrap() = None;
        std::fs::remove_dir_all(&a).unwrap();
        assert_eq!(read_recent(&sh), vec![root_b]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

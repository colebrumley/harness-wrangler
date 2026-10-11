//! Desktop shell. The backend is wrangler-core called in-process: every UI
//! action goes through `api`, the same command surface `wrangler serve`
//! exposes over HTTP. The shell itself only owns which project is open.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};
use wrangler_core::api::Api;
use wrangler_core::Project;

#[derive(Default)]
struct Shell {
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

#[tauri::command]
fn project_open(path: String, shell: State<Shell>) -> Result<String, String> {
    let dir = Path::new(&path);
    // Accept the project folder itself or anything inside it.
    let project = Project::discover(dir).map_err(|_| format!("{path} is not a Harness Wrangler project (no wrangler.toml or experiments/ folder). Use \"Create project in a folder\" to start one there."))?;
    open_project(&shell, project)
}

#[tauri::command]
fn project_init(path: String, shell: State<Shell>) -> Result<String, String> {
    let project = Project::init(Path::new(&path)).map_err(|e| format!("{e:#}"))?;
    open_project(&shell, project)
}

#[tauri::command]
fn project_current(shell: State<Shell>) -> Option<String> {
    shell.api.lock().unwrap().as_ref().map(|a| a.project.root.to_string_lossy().to_string())
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
/// so `claude`, `codex` and `git` would not be found. Take PATH from a login
/// shell once at startup.
fn adopt_login_shell_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = std::process::Command::new(&shell).args(["-lic", "printf '__PATH__%s__END__' \"$PATH\""]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(path) = text.split("__PATH__").nth(1).and_then(|r| r.split("__END__").next()) {
            if !path.is_empty() {
                std::env::set_var("PATH", path);
            }
        }
    }
}

pub fn run() {
    adopt_login_shell_path();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Shell::default())
        .setup(|app| {
            let shell = app.state::<Shell>();
            *shell.config_dir.lock().unwrap() = app.path().app_config_dir().ok();
            // Open straight into a project when one is given or was open last time.
            let candidates: Vec<String> = std::env::args().nth(1).into_iter().chain(std::env::var("WRANGLER_PROJECT").ok()).chain(read_recent(&shell).into_iter().take(1)).collect();
            for c in candidates {
                if let Ok(project) = Project::discover(Path::new(&c)) {
                    if open_project(&shell, project).is_ok() {
                        break;
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![project_open, project_init, project_current, project_close, recent_projects, api])
        .run(tauri::generate_context!())
        .expect("error while running Harness Wrangler");
}

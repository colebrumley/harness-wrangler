//! `wrangler serve`: the core API over HTTP on localhost, plus the built web
//! UI as static files. The desktop app calls the same API in-process; this
//! mode exists for browsers, scripts and UI development.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};
use wrangler_core::api::Api;
use wrangler_core::Project;

fn ui_dir(explicit: Option<PathBuf>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    candidates.extend(explicit);
    candidates.extend(std::env::var("WRANGLER_UI_DIR").ok().map(PathBuf::from));
    if let Ok(exe) = std::env::current_exe() {
        // target/<profile>/wrangler → repo root
        if let Some(root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            candidates.push(root.join("apps/desktop/dist"));
        }
    }
    candidates.into_iter().find(|p| p.join("index.html").is_file())
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn respond_json(req: Request, status: u16, body: &Value) {
    let _ = req.respond(Response::from_string(body.to_string()).with_status_code(status).with_header(header("Content-Type", "application/json")).with_header(header("Cache-Control", "no-store")));
}

fn handle(api: &Arc<Api>, ui: &Option<PathBuf>, port: u16, mut req: Request) {
    let url = req.url().split('?').next().unwrap_or("/").to_string();
    if let Some(cmd) = url.strip_prefix("/api/") {
        // The API can write project files, so refuse anything a foreign web
        // page could send: cross-origin requests and non-JSON content types.
        let get = |name: &str| req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string());
        let own = [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}")];
        let dev = ["http://localhost:1420".to_string(), "http://127.0.0.1:1420".to_string()];
        if let Some(origin) = get("Origin") {
            if !own.contains(&origin) && !dev.contains(&origin) {
                return respond_json(req, 403, &json!({"ok": false, "error": "cross-origin request refused"}));
            }
        }
        if *req.method() != Method::Post || !get("Content-Type").map(|c| c.starts_with("application/json")).unwrap_or(false) {
            return respond_json(req, 400, &json!({"ok": false, "error": "POST application/json only"}));
        }
        let mut body = String::new();
        if req.as_reader().take(32 * 1024 * 1024).read_to_string(&mut body).is_err() {
            return respond_json(req, 400, &json!({"ok": false, "error": "unreadable body"}));
        }
        let args: Value = if body.trim().is_empty() { json!({}) } else { serde_json::from_str(&body).unwrap_or(json!({})) };
        let cmd = cmd.to_string();
        match api.dispatch(&cmd, &args) {
            Ok(data) => respond_json(req, 200, &json!({"ok": true, "data": data})),
            Err(e) => respond_json(req, 200, &json!({"ok": false, "error": format!("{e:#}")})),
        }
        return;
    }
    let Some(ui) = ui else {
        let _ = req.respond(Response::from_string("The web UI is not built. Run `npm install && npm run build` in apps/desktop, or pass --ui <dir>. The JSON API is at POST /api/<command>.").with_status_code(404));
        return;
    };
    let rel = url.trim_start_matches('/');
    let mut path = ui.join(if rel.is_empty() { "index.html" } else { rel });
    let inside = path.canonicalize().ok().zip(ui.canonicalize().ok()).map(|(p, u)| p.starts_with(u)).unwrap_or(false);
    if !inside || !path.is_file() {
        // Single-page app: unknown paths get the shell.
        path = ui.join("index.html");
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            let _ = req.respond(Response::from_data(bytes).with_header(header("Content-Type", mime(&path))));
        }
        Err(_) => {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

pub fn serve(project: Project, port: u16, ui: Option<PathBuf>) -> Result<()> {
    let api = Api::new(project)?;
    let ui = ui_dir(ui);
    let server = Arc::new(Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("binding 127.0.0.1:{port}: {e}"))?);
    eprintln!("wrangler serving {} on http://127.0.0.1:{port}", api.project.root.display());
    match &ui {
        Some(d) => eprintln!("web UI from {}", d.display()),
        None => eprintln!("web UI not built (API only). Build it with: cd apps/desktop && npm install && npm run build"),
    }
    let mut workers = vec![];
    for _ in 0..8 {
        let (server, api, ui) = (server.clone(), api.clone(), ui.clone());
        workers.push(std::thread::spawn(move || {
            while let Ok(req) = server.recv() {
                handle(&api, &ui, port, req);
            }
        }));
    }
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

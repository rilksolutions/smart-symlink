// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dirlinker_core::{fsinfo, links, listing, ops, size, util};
use std::cell::Cell;
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Size computations from the GUI use slots 0 (left pane) and 1 (right pane).
fn slot(s: usize) -> usize {
    s.min(1)
}

#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
fn home_dir() -> Option<String> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(|h| util::display_path(Path::new(&h)))
}

#[tauri::command]
async fn list_drives() -> Vec<fsinfo::DriveInfo> {
    tauri::async_runtime::spawn_blocking(fsinfo::list_drives).await.unwrap_or_default()
}

#[tauri::command]
async fn list_dir(path: String) -> Result<listing::Listing, String> {
    tauri::async_runtime::spawn_blocking(move || listing::list_dir(&path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn dir_size(path: String, pane: usize) -> Result<size::SizeInfo, String> {
    tauri::async_runtime::spawn_blocking(move || size::dir_size(Path::new(&path), Some(slot(pane))))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "cancelled".to_string())
}

#[tauri::command]
fn cancel_sizes(pane: usize) {
    size::cancel(slot(pane));
}

#[tauri::command]
fn invalidate_sizes(path: Option<String>) {
    match path {
        Some(p) => size::invalidate(Path::new(&p)),
        None => size::clear_cache(),
    }
}

#[tauri::command]
async fn link_options(sources: Vec<String>, dest: String, mode: links::Mode) -> links::Analysis {
    tauri::async_runtime::spawn_blocking(move || links::analyze(&sources, &dest, mode))
        .await
        .expect("analysis task panicked")
}

#[tauri::command]
async fn execute(app: AppHandle, req: ops::Request) -> Result<Vec<ops::ItemResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let last = Cell::new(Instant::now() - Duration::from_secs(1));
        let last_phase = std::cell::RefCell::new(String::new());
        ops::execute(&req, &|p| {
            // Throttle copy progress; always send phase changes.
            let phase_changed = *last_phase.borrow() != p.phase;
            if phase_changed || last.get().elapsed() >= Duration::from_millis(80) {
                last.set(Instant::now());
                *last_phase.borrow_mut() = p.phase.clone();
                let _ = app.emit("op-progress", p);
            }
        })
    })
    .await
    .map_err(|e| e.to_string())
}

/// Show a path in the system file manager.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = util::normalize_input(&path);
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", util::display_path(&p))).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(p.as_path())).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            platform,
            home_dir,
            list_drives,
            list_dir,
            dir_size,
            cancel_sizes,
            invalidate_sizes,
            link_options,
            execute,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running DirLinker");
}

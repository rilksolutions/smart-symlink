// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use smart_symlink_core::{fsinfo, links, listing, ops, registry, size, util};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Size computations from the GUI use slots 0 (left pane) and 1 (right pane).
fn slot(s: usize) -> usize {
    s.min(1)
}

/// Run a blocking job off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Progress callback that forwards to the UI (throttled, phase changes always sent).
fn progress_emitter(app: AppHandle) -> impl Fn(&ops::Progress) {
    let last = Cell::new(Instant::now() - Duration::from_secs(1));
    let last_key = RefCell::new(String::new());
    move |p: &ops::Progress| {
        let key = format!("{}:{}", p.item_index, p.phase);
        if *last_key.borrow() != key || last.get().elapsed() >= Duration::from_millis(80) {
            last.set(Instant::now());
            *last_key.borrow_mut() = key;
            let _ = app.emit("op-progress", p);
        }
    }
}

#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
fn home_dir() -> Option<String> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| util::display_path(Path::new(&h)))
}

#[tauri::command]
async fn list_drives() -> Vec<fsinfo::DriveInfo> {
    blocking(fsinfo::list_drives).await.unwrap_or_default()
}

#[tauri::command]
async fn list_dir(path: String) -> Result<listing::Listing, String> {
    blocking(move || listing::list_dir(&path)).await?
}

#[tauri::command]
async fn entry_info(path: String) -> Result<listing::Entry, String> {
    blocking(move || {
        let p = util::normalize_input(&path);
        let md = std::fs::symlink_metadata(&p).map_err(|e| listing::friendly_io(&e, &p))?;
        Ok(listing::make_entry(&p, &md, true))
    })
    .await?
}

#[tauri::command]
async fn dir_size(path: String, pane: usize) -> Result<size::SizeInfo, String> {
    blocking(move || size::dir_size(Path::new(&path), Some(slot(pane))))
        .await?
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
async fn link_options(sources: Vec<String>, dest: String, mode: links::Mode) -> Result<links::Analysis, String> {
    blocking(move || links::analyze(&sources, &dest, mode)).await
}

#[tauri::command]
async fn execute(app: AppHandle, req: ops::Request) -> Result<Vec<ops::ItemResult>, String> {
    blocking(move || ops::execute(&req, &progress_emitter(app))).await
}

#[tauri::command]
async fn unlink(app: AppHandle, links: Vec<String>, mode: ops::UnlinkMode, verify: bool) -> Result<Vec<ops::ItemResult>, String> {
    blocking(move || ops::unlink(&links, mode, verify, &progress_emitter(app))).await
}

#[tauri::command]
async fn move_items(app: AppHandle, sources: Vec<String>, dest: String, verify: bool) -> Result<Vec<ops::ItemResult>, String> {
    blocking(move || ops::move_items(&sources, &dest, verify, &progress_emitter(app))).await
}

#[tauri::command]
async fn delete_items(app: AppHandle, paths: Vec<String>, permanent: bool) -> Result<Vec<ops::ItemResult>, String> {
    blocking(move || ops::delete(&paths, permanent, &progress_emitter(app))).await
}

#[tauri::command]
async fn create_dir(parent: String, name: String) -> Result<String, String> {
    blocking(move || ops::create_dir(&parent, &name)).await?
}

#[tauri::command]
async fn rename_item(path: String, name: String) -> Result<String, String> {
    blocking(move || ops::rename(&path, &name)).await?
}

/// Sync on purpose: Tauri runs sync commands on the main (UI) thread, whose message
/// loop the Windows properties dialog needs.
#[tauri::command]
fn native_properties(path: String) -> Result<bool, String> {
    ops::native_properties(&path)
}

/// Restart elevated ("Run as administrator") so owners and auditing entries can be kept.
#[tauri::command]
fn restart_elevated(app: AppHandle) -> Result<(), String> {
    if smart_symlink_core::meta::restart_elevated()? {
        app.exit(0);
    }
    Ok(())
}

/// Hint shown in dialogs when some metadata can only be kept with elevated rights.
#[tauri::command]
fn privileges_note() -> Option<&'static str> {
    smart_symlink_core::meta::privileges_note()
}

#[tauri::command]
async fn saved_links() -> Result<Vec<registry::SavedLinkView>, String> {
    blocking(registry::list_links).await
}

#[tauri::command]
fn forget_link(link: String) -> Result<(), String> {
    registry::forget_link(&link).map(|_| ())
}

#[tauri::command]
fn favorites() -> Vec<String> {
    registry::load().favorites
}

#[tauri::command]
fn set_favorite(path: String, on: bool) -> Result<Vec<String>, String> {
    registry::set_favorite(&path, on).map(|s| s.favorites)
}

/// Saved links whose data lives at or below `path` (to warn before rename/delete/move).
#[tauri::command]
fn links_into(path: String) -> Vec<registry::SavedLink> {
    registry::links_into(&path)
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
            entry_info,
            dir_size,
            cancel_sizes,
            invalidate_sizes,
            link_options,
            execute,
            unlink,
            move_items,
            delete_items,
            create_dir,
            rename_item,
            native_properties,
            restart_elevated,
            privileges_note,
            saved_links,
            forget_link,
            favorites,
            set_favorite,
            links_into,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running Smart Symlink");
}

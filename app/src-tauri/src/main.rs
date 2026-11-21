// Lumen — the macOS shell around lumen-core.
//
// Responsibilities: own the index and config, run scans and content reading
// off the main thread and stream progress to the UI, keep the FSEvents watcher
// alive, expose Finder actions (open, reveal, Quick Look, move to Trash), the
// menu bar icon and the configurable summon hotkey.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lumen_core::pace::{self, Hold};
use lumen_core::{
    index_content, index_semantic, scan, semantic_search, set_embed_helper, set_ocr_helper, CollectionStat, Config, DuplicateGroup, FileRecord, Index, IndexStatus, Kind, MonthStat, Overview, Suggestion,
    SearchRequest, SearchResponse, SemanticRequest, SemanticResponse, Watcher,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, Theme, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_updater::UpdaterExt;

struct AppState {
    index: Arc<Index>,
    config: Mutex<Config>,
    config_path: PathBuf,
    scanning: Arc<AtomicBool>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    watcher: Mutex<Option<Watcher>>,
    reading: Arc<AtomicBool>,
    read_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Stops the duplicates view's hashing (v0.9): hashing many huge
    /// same-size files in full the first time could not be stopped.
    dup_cancel: Mutex<Option<Arc<AtomicBool>>>,
    embedding: Arc<AtomicBool>,
    /// A scan was asked for (new settings, a drive plugged in, a rebuild)
    /// while one was running: run another when it ends, with the new config.
    rescan: Arc<AtomicBool>,
    /// The same for reading: a scan added files while a reading pass (whose
    /// list was already made) was running.
    reread: Arc<AtomicBool>,
    /// rebuild_index is stopping work and wiping the index: nothing may start
    /// a watcher or a reading pass on the old rows until it has.
    rebuilding: AtomicBool,
    /// The index file was damaged at launch and has been started over (v0.8).
    rebuilt: AtomicBool,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    status: IndexStatus,
    config: Config,
    scanning: bool,
    first_run: bool,
    home: String,
    reading: bool,
    /// Full Disk Access granted (macOS). Without it every protected folder
    /// asks separately and some (Mail downloads, iCloud) stay hidden.
    full_disk_access: bool,
    version: String,
    /// Semantic search can run here (helper present and switched on).
    semantic_ready: bool,
    /// Files with a vector.
    semantic_files: i64,
    /// Why background reading is holding still, if it is (v0.8).
    hold: Option<Hold>,
    /// The index was damaged and is being rebuilt: say so once, skip onboarding.
    index_rebuilt: bool,
}

#[tauri::command(async)]
fn snapshot(app: AppHandle, window: tauri::Window, state: State<AppState>) -> Res<Snapshot> {
    let status = state.index.status().map_err(err)?;
    Ok(Snapshot {
        first_run: status.generation == 0,
        status,
        config: state.config.lock().unwrap().clone(),
        scanning: state.scanning.load(Ordering::Relaxed),
        home: lumen_home(),
        reading: state.reading.load(Ordering::Relaxed),
        full_disk_access: has_full_disk_access(),
        version: app.package_info().version.to_string(),
        semantic_ready: lumen_core::semantic::available(),
        semantic_files: lumen_core::semantic::semantic_count(&state.index).unwrap_or(0),
        hold: pace::hold(),
        // Only the main window says so (and clears it): the hidden quick panel
        // also asks for a snapshot at launch and would otherwise swallow it.
        index_rebuilt: if window.label() == "main" { state.rebuilt.swap(false, Ordering::Relaxed) } else { state.rebuilt.load(Ordering::Relaxed) },
    })
}

fn lumen_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

#[tauri::command(async)]
fn search(state: State<AppState>, req: SearchRequest) -> Res<SearchResponse> {
    state.index.search(&req).map_err(err)
}

/// Find files by what they are about (v0.6 beta).
#[tauri::command]
async fn semantic(state: State<'_, AppState>, req: SemanticRequest) -> Res<SemanticResponse> {
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || semantic_search(&index, &req))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command(async)]
fn overview(state: State<AppState>) -> Res<Overview> {
    state.index.overview().map_err(err)
}

#[tauri::command(async)]
fn recent(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.recent(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command(async)]
fn largest(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.largest(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command(async)]
fn forgotten(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.forgotten(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command]
async fn duplicates(state: State<'_, AppState>) -> Res<Vec<DuplicateGroup>> {
    let index = state.index.clone();
    // A new look replaces (and stops) the previous one.
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(old) = state.dup_cancel.lock().unwrap().replace(cancel.clone()) {
        old.store(true, Ordering::Relaxed);
    }
    let stop = cancel.clone();
    let groups = tauri::async_runtime::spawn_blocking(move || index.duplicates_cancellable(1024 * 1024, 40, &stop))
        .await
        .map_err(err)?
        .map_err(err)?;
    // Stopped: what was found is incomplete, and the UI says so instead.
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    Ok(groups)
}

/// Stop the duplicates view's hashing; finished hashes stay cached.
#[tauri::command]
fn cancel_duplicates(state: State<AppState>) {
    if let Some(c) = state.dup_cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>) -> Res<()> {
    run_scan(&app, &state)
}

/// Clears a busy flag when dropped, so a panic or an early return can never
/// leave Lumen thinking a scan or a reading pass is still running.
struct Busy(Arc<AtomicBool>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A scan that must see the current settings: when one is already running
/// (with the old roots, exclusions or drives), run another right after it.
/// Plain `run_scan` joined the running scan and the change was not indexed
/// until the next launch.
fn run_scan_fresh(app: &AppHandle, state: &AppState) -> Res<()> {
    if state.scanning.load(Ordering::SeqCst) {
        state.rescan.store(true, Ordering::SeqCst);
    }
    run_scan(app, state)
}

fn run_scan(app: &AppHandle, state: &AppState) -> Res<()> {
    if state.scanning.swap(true, Ordering::SeqCst) {
        return Ok(()); // already running; the UI is subscribed to its events
    }
    let busy = Busy(state.scanning.clone());
    // This scan reads the settings as they are now: an earlier request for
    // a fresh scan is answered by it.
    state.rescan.store(false, Ordering::SeqCst);
    let rescan = state.rescan.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    *state.cancel.lock().unwrap() = Some(cancel.clone());
    let index = state.index.clone();
    let cfg = state.config.lock().unwrap().clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("lumen-scan".into())
        .spawn(move || {
            let emitter = app.clone();
            let cancel_sem = cancel.clone();
            let result = scan(&index, &cfg, cancel, |p| {
                let _ = emitter.emit("scan://progress", p);
            });
            drop(busy);
            // Settings changed (or a drive came) while this scan ran: scan
            // again with them instead of reading with the old ones.
            // The UI keeps showing one scan: no "done" for this one, the next
            // one's progress and result follow.
            if rescan.swap(false, Ordering::SeqCst) {
                let state = app.state::<AppState>();
                // (A rebuild scans by itself once the index is wiped.)
                if !state.rebuilding.load(Ordering::SeqCst) {
                    if let Err(e) = run_scan(&app, &state) {
                        let _ = app.emit("scan://error", e);
                    }
                }
                return;
            }
            match result {
                Ok(report) => {
                    let _ = app.emit("scan://done", &report);
                    restart_watcher(&app);
                    // Warm the whole-index summaries while the user is still
                    // reading the scan result, so Insights, Timeline and the
                    // collections open instantly (they stay cached until the
                    // index changes).
                    if !report.cancelled {
                        let warm = index.clone();
                        let _ = std::thread::Builder::new().name("lumen-warm".into()).spawn(move || {
                            pace::background_thread();
                            let _ = warm.overview();
                            let _ = warm.collections();
                            let _ = warm.timeline(1);
                        });
                    }
                    if !report.cancelled && cfg.content_search {
                        run_content(&app);
                    } else if !report.cancelled {
                        run_semantic(&app, cancel_sem);
                    }
                }
                Err(e) => {
                    let _ = app.emit("scan://error", e.to_string());
                }
            }
        })
        .map_err(err)?; // (the closure, and `busy` with it, is dropped on failure)
    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    // Stopped means stopped: no queued scan or reading pass starts after.
    state.rescan.store(false, Ordering::SeqCst);
    state.reread.store(false, Ordering::SeqCst);
    if let Some(c) = state.cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
    if let Some(c) = state.read_cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

/// Read the words of new and changed documents, after a scan.
fn run_content(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.rebuilding.load(Ordering::SeqCst) {
        return;
    }
    if state.reading.swap(true, Ordering::SeqCst) {
        // The running pass will not see this scan's new files: read again
        // after it, rather than at the next launch.
        state.reread.store(true, Ordering::SeqCst);
        return;
    }
    let busy = Busy(state.reading.clone());
    state.reread.store(false, Ordering::SeqCst);
    let reread = state.reread.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    *state.read_cancel.lock().unwrap() = Some(cancel.clone());
    let index = state.index.clone();
    let app = app.clone();
    let _ = std::thread::Builder::new().name("lumen-content".into()).spawn(move || {
        pace::background_thread();
        let emitter = app.clone();
        let r = index_content(&index, cancel.clone(), |p| {
            let _ = emitter.emit("content://progress", p);
        });
        drop(busy);
        match r {
            Ok(report) => {
                let _ = app.emit("content://done", &report);
            }
            Err(e) => log::warn!("content indexing failed: {e}"),
        }
        if !cancel.load(Ordering::Relaxed) && reread.swap(false, Ordering::SeqCst) {
            run_content(&app);
            return;
        }
        // Vectors use the first lines of each file's text, so they come after.
        if !cancel.load(Ordering::Relaxed) {
            run_semantic(&app, cancel);
        }
    });
}

/// Embed new and changed files for semantic search, on the content thread.
fn run_semantic(app: &AppHandle, cancel: Arc<AtomicBool>) {
    if !lumen_core::semantic::available() {
        return;
    }
    let state = app.state::<AppState>();
    if state.rebuilding.load(Ordering::SeqCst) || state.embedding.swap(true, Ordering::SeqCst) {
        return;
    }
    let busy = Busy(state.embedding.clone());
    let emitter = app.clone();
    let r = index_semantic(&state.index, cancel, |p| {
        let _ = emitter.emit("semantic://progress", p);
    });
    drop(busy);
    match r {
        Ok(report) => {
            let _ = app.emit("semantic://done", &report);
        }
        Err(e) => log::warn!("semantic indexing failed: {e}"),
    }
}

fn restart_watcher(app: &AppHandle) {
    let state = app.state::<AppState>();
    // A scan that ends while the index is being rebuilt must not bring the
    // watcher back on rows about to be wiped; the rebuild's scan starts it.
    if state.rebuilding.load(Ordering::SeqCst) {
        return;
    }
    let cfg = state.config.lock().unwrap().clone();
    let emitter = app.clone();
    let mut slot = state.watcher.lock().unwrap();
    *slot = None; // stop the old stream first
    match Watcher::start(&state.index, &cfg, move |batch| {
        let plugged = batch.volumes_changed;
        let left = batch.content_left;
        let _ = emitter.emit("index://changed", batch);
        if left && !plugged {
            // A big folder moved in: the watcher read the first documents,
            // a paced background pass reads the rest now instead of at the
            // next scan (often the next launch).
            run_content(&emitter);
        }
        if plugged {
            // A drive came or went: pick up its files, or mark them offline.
            let app = emitter.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let state = app.state::<AppState>();
                let _ = run_scan_fresh(&app, &state);
            });
        }
    }) {
        Ok(w) => *slot = Some(w),
        Err(e) => log::warn!("watcher failed to start: {e}"),
    }
}

#[tauri::command(async)]
fn set_roots(app: AppHandle, state: State<AppState>, roots: Vec<String>) -> Res<Config> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        cfg.roots = roots.into_iter().map(PathBuf::from).collect();
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    run_scan_fresh(&app, &state)?;
    Ok(cfg)
}

#[tauri::command(async)]
fn set_excluded(app: AppHandle, state: State<AppState>, paths: Vec<String>) -> Res<Config> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        cfg.excluded_paths = paths.into_iter().map(PathBuf::from).collect();
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    run_scan_fresh(&app, &state)?;
    Ok(cfg)
}

/// Folder names and "*.ext" patterns to skip (v0.8).
#[tauri::command(async)]
fn set_excluded_names(app: AppHandle, state: State<AppState>, names: Vec<String>) -> Res<Config> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        let mut names: Vec<String> = names.into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
        names.dedup();
        cfg.excluded_names = names;
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    run_scan_fresh(&app, &state)?;
    Ok(cfg)
}

/// Pause or resume background reading (text, OCR, vectors).
#[tauri::command]
fn set_paused(app: AppHandle, on: bool) -> Option<Hold> {
    pace::set_paused(on);
    let h = pace::hold();
    let _ = app.emit("pace://hold", h);
    h
}

/// Searches to try after the first scan, from the user's own files.
#[tauri::command(async)]
fn suggestions(state: State<AppState>) -> Res<Vec<Suggestion>> {
    state.index.suggestions(3).map_err(err)
}

#[tauri::command(async)]
fn recently_opened(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.recently_opened(limit.unwrap_or(20)).map_err(err)
}

/// Start the index over from nothing: stop all work, forget every row, scan.
#[tauri::command]
async fn rebuild_index(app: AppHandle) -> Res<()> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        state.rebuilding.store(true, Ordering::SeqCst);
        for slot in [&state.cancel, &state.read_cancel, &state.dup_cancel] {
            if let Some(c) = slot.lock().unwrap().as_ref() {
                c.store(true, Ordering::Relaxed);
            }
        }
        *state.watcher.lock().unwrap() = None;
        // Let running passes notice the cancel and let go of the database.
        for _ in 0..200 {
            if !state.scanning.load(Ordering::SeqCst) && !state.reading.load(Ordering::SeqCst) && !state.embedding.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // A scan that finished just as the cancel landed may have started
        // the watcher again before it saw `rebuilding`.
        *state.watcher.lock().unwrap() = None;
        let reset = state.index.reset().map_err(err);
        state.rebuilding.store(false, Ordering::SeqCst);
        reset?;
        // A scan still running (it did not stop in time) is followed by a
        // fresh one; before, the rebuild then scanned nothing at all.
        run_scan_fresh(&handle, &state)
    })
    .await
    .map_err(err)?
}

/// Smart collections and how many files each holds.
#[tauri::command(async)]
fn collections(state: State<AppState>) -> Res<Vec<CollectionStat>> {
    state.index.collections().map_err(err)
}

#[tauri::command(async)]
fn collection(state: State<AppState>, id: String, limit: Option<usize>, offset: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.collection(&id, limit.unwrap_or(200), offset.unwrap_or(0)).map_err(err)
}

/// Months that have files, newest first.
#[tauri::command(async)]
fn timeline(state: State<AppState>) -> Res<Vec<MonthStat>> {
    state.index.timeline(240).map_err(err)
}

#[tauri::command(async)]
fn month(state: State<AppState>, month: String, kind: Option<Kind>, limit: Option<usize>, offset: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.month(&month, kind, limit.unwrap_or(300), offset.unwrap_or(0)).map_err(err)
}

/// Save the share card PNG to the Desktop and show it in Finder.
#[tauri::command(async)]
fn save_share_card(png: Vec<u8>) -> Res<String> {
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" || png.len() > 20 << 20 {
        return Err("not a PNG".into());
    }
    let home = std::env::var("HOME").map_err(err)?;
    let desk = Path::new(&home).join("Desktop");
    let dir = if desk.is_dir() { desk } else { PathBuf::from(&home) };
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Never over an earlier card: two saves in the same second used to
    // share a name, and the second silently replaced the first.
    let mut n = 0;
    let path = loop {
        let p = dir.join(if n == 0 { format!("Lumen-{stamp}.png") } else { format!("Lumen-{stamp}-{n}.png") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(&png) {
                    let _ = std::fs::remove_file(&p);
                    return Err(err(e));
                }
                break p;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(e) => return Err(err(e)),
        }
    };
    let _ = Command::new("open").arg("-R").arg(&path).spawn();
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Serialize, Clone)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: String,
}

/// Ask GitHub Releases whether a newer Lumen exists (signed latest.json).
#[tauri::command]
async fn check_update(app: AppHandle) -> Res<Option<UpdateInfo>> {
    let update = app.updater().map_err(err)?.check().await.map_err(err)?;
    Ok(update.map(|u| UpdateInfo { version: u.version.clone(), current: u.current_version.clone(), notes: u.body.clone().unwrap_or_default() }))
}

/// Download, verify the signature, replace the app and relaunch.
#[tauri::command]
async fn install_update(app: AppHandle) -> Res<()> {
    let Some(update) = app.updater().map_err(err)?.check().await.map_err(err)? else { return Ok(()) };
    let app2 = app.clone();
    let mut got: usize = 0;
    update
        .download_and_install(
            move |chunk, total| {
                got += chunk;
                if let Some(t) = total {
                    let _ = app2.emit("update-progress", (got as f64 / t as f64).min(1.0));
                }
            },
            || {},
        )
        .await
        .map_err(err)?;
    relaunch_soon(&app);
    Ok(())
}

/// The Vision OCR helper ships next to the app binary (Contents/MacOS).
fn use_ocr(on: bool) {
    let helper = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lumen-ocr")));
    set_ocr_helper(if on { helper } else { None });
}

/// The same helper embeds sentences (NaturalLanguage) for semantic search.
fn use_semantic(on: bool) {
    let helper = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lumen-ocr")));
    set_embed_helper(if on { helper } else { None });
}

/// Content search and external drives on or off.
#[tauri::command(async)]
fn set_options(
    app: AppHandle,
    state: State<AppState>,
    content_search: Option<bool>,
    include_external: Option<bool>,
    ocr: Option<bool>,
    quick_panel: Option<bool>,
    launch_at_login: Option<bool>,
    theme: Option<String>,
    auto_update: Option<bool>,
    semantic: Option<bool>,
    include_icloud: Option<bool>,
    include_cloud_storage: Option<bool>,
    pause_on_battery: Option<bool>,
) -> Res<Config> {
    // The changes go to a copy, which becomes the running settings only once
    // it is saved (v0.9). Before, a failed save turned the switch back in the
    // UI while the running settings kept the new value, and the next
    // successful save wrote it to disk anyway. The lock is held throughout,
    // so two changes at once cannot save over each other.
    let cfg = {
        let mut running = state.config.lock().unwrap();
        let mut cfg = running.clone();
        if let Some(v) = content_search {
            cfg.content_search = v;
        }
        if let Some(v) = include_external {
            cfg.include_external = v;
        }
        if let Some(v) = ocr {
            cfg.ocr = v;
        }
        if let Some(v) = quick_panel {
            cfg.quick_panel = v;
        }
        if let Some(v) = launch_at_login {
            cfg.launch_at_login = v;
        }
        if let Some(v) = &theme {
            cfg.theme = v.clone();
        }
        if let Some(v) = auto_update {
            cfg.auto_update = v;
        }
        if let Some(v) = include_icloud {
            cfg.include_icloud = v;
        }
        if let Some(v) = include_cloud_storage {
            cfg.include_cloud_storage = v;
        }
        if let Some(v) = pause_on_battery {
            cfg.pause_on_battery = v;
        }
        if let Some(v) = semantic {
            cfg.semantic = v;
        }
        // Launch at login is set before saving, so a refusal from macOS saves
        // nothing; it is set back if the save then fails.
        let login_before = match launch_at_login {
            Some(on) => {
                let al = app.autolaunch();
                let before = al.is_enabled().unwrap_or(running.launch_at_login);
                if on { al.enable() } else { al.disable() }.map_err(err)?;
                Some(before)
            }
            None => None,
        };
        if let Err(e) = cfg.save(&state.config_path) {
            if let Some(before) = login_before {
                let al = app.autolaunch();
                if let Err(e) = if before { al.enable() } else { al.disable() } {
                    log::warn!("could not set launch at login back: {e}");
                }
            }
            return Err(err(e));
        }
        *running = cfg.clone();
        cfg
    };
    // Saved: now the changes take effect.
    if let Some(v) = ocr {
        use_ocr(v);
    }
    if let Some(v) = pause_on_battery {
        pace::set_pause_on_battery(v);
        let _ = app.emit("pace://hold", pace::hold());
    }
    if let Some(v) = semantic {
        use_semantic(v);
        if !v {
            let _ = lumen_core::semantic::clear_vectors(&state.index);
        }
    }
    if theme.is_some() {
        apply_theme(&app, &cfg.theme);
    }
    // Only the options that change what is indexed need a scan.
    if content_search.is_some()
        || include_external.is_some()
        || include_icloud.is_some()
        || include_cloud_storage.is_some()
        || ocr.is_some()
        || semantic == Some(true)
    {
        // The setting is saved and in effect: a scan that cannot start now
        // (one already running is queued) must not report the change as
        // failed, or the UI would turn back a switch that did change.
        if let Err(e) = run_scan_fresh(&app, &state) {
            log::warn!("rescan after a settings change did not start: {e}");
        }
    }
    Ok(cfg)
}

fn apply_theme(app: &AppHandle, theme: &str) {
    let t = match theme {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    };
    for w in app.webview_windows().values() {
        let _ = w.set_theme(t);
    }
    let _ = app.emit("app://theme", theme);
}

// ------------------------------------------------------------ quick panel --

const QUICK_WIDTH: f64 = 680.0;

/// The shortcut and the menu bar icon: the quick panel, or the main window
/// when the user turned the panel off.
fn summon(app: &AppHandle) {
    let quick = app.state::<AppState>().config.lock().unwrap().quick_panel;
    if quick {
        toggle_quick(app)
    } else {
        toggle_main(app)
    }
}

fn toggle_quick(app: &AppHandle) {
    let Some(w) = app.get_webview_window("quick") else { return toggle_main(app) };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return;
    }
    // Centered, a fifth of the way down the screen the cursor is on, like Spotlight.
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| w.current_monitor().ok().flatten());
    if let Some(m) = monitor {
        let scale = m.scale_factor();
        let (mp, ms) = (m.position(), m.size());
        let x = mp.x + ((ms.width as f64 - QUICK_WIDTH * scale) / 2.0) as i32;
        let y = mp.y + (ms.height as f64 * 0.2) as i32;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
    let _ = w.emit("quick://show", ());
}

/// The panel's page measures itself after every render.
#[tauri::command]
fn quick_resize(app: AppHandle, height: f64) {
    if let Some(w) = app.get_webview_window("quick") {
        let _ = w.set_size(LogicalSize::new(QUICK_WIDTH, height.clamp(62.0, 640.0)));
    }
}

#[tauri::command]
fn quick_hide(app: AppHandle) {
    if let Some(w) = app.get_webview_window("quick") {
        let _ = w.hide();
    }
}

/// "See all in Lumen": hand the query to the main window.
#[tauri::command]
fn show_in_main(app: AppHandle, query: String) {
    quick_hide(app.clone());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        // To this window only: `emit` goes to every window, and the panel
        // (which also listens for app://query) re-ran the query behind it.
        let _ = w.emit_to("main", "app://query", query);
    }
}

/// A lumen:// link that arrived before the window was ready to hear it.
static PENDING_LINK: Mutex<Option<Link>> = Mutex::new(None);

#[derive(Clone, Serialize)]
struct Link {
    /// "search" or "ask"
    mode: String,
    query: String,
}

/// lumen://search?q=… · lumen://ask?q=… · lumen://quick?q=… (Raycast,
/// Alfred, Shortcuts' "Open URL", or a link in a note).
fn open_link(app: &AppHandle, url: &tauri::Url) {
    if url.scheme() != "lumen" {
        return;
    }
    let host = url.host_str().unwrap_or("search").to_ascii_lowercase();
    let query = url.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()).unwrap_or_default();
    if host == "quick" {
        if !app.get_webview_window("quick").map(|w| w.is_visible().unwrap_or(false)).unwrap_or(false) {
            toggle_quick(app);
        }
        // To the panel only: `emit` reaches every window, and the main
        // window switched to search with the panel's query, losing its view.
        let _ = app.emit_to("quick", "app://query", query);
        return;
    }
    let link = Link { mode: if host == "ask" { "ask".into() } else { "search".into() }, query };
    *PENDING_LINK.lock().unwrap() = Some(link.clone());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        // A running window takes it now (and clears it via take_link); a
        // window still loading asks for it with take_link once it is ready.
        let _ = w.emit("app://link", &link);
    }
}

#[tauri::command]
fn take_link() -> Option<Link> {
    PENDING_LINK.lock().unwrap().take()
}

/// Change the summon shortcut. The old one stays if the new one is taken.
#[tauri::command]
fn set_hotkey(app: AppHandle, state: State<AppState>, hotkey: String) -> Res<Config> {
    let new: Shortcut = hotkey.parse().map_err(|_| "invalid".to_string())?;
    let old = state.config.lock().unwrap().hotkey.clone();
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if gs.register(new).is_err() {
        if let Ok(o) = old.parse::<Shortcut>() {
            let _ = gs.register(o);
        }
        return Err("taken".into());
    }
    let mut cfg = state.config.lock().unwrap();
    cfg.hotkey = hotkey;
    cfg.save(&state.config_path).map_err(err)?;
    Ok(cfg.clone())
}

#[derive(Serialize)]
struct Trashed {
    count: usize,
    bytes: u64,
}

/// Move files to the Trash (Finder's "Put Back" restores them) and forget
/// them. Used by the duplicates view.
#[tauri::command(async)]
fn trash_paths(state: State<AppState>, paths: Vec<String>) -> Res<Trashed> {
    let present: Vec<&String> = paths.iter().filter(|p| Path::new(p).exists()).collect();
    let bytes = present.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    trash::delete_all(&present).map_err(err)?;
    for p in &paths {
        let _ = state.index.remove_path(p);
    }
    Ok(Trashed { count: present.len(), bytes })
}

/// Probe a file only readable with Full Disk Access. Not found anywhere means
/// we cannot tell, which we treat as granted rather than nag.
fn has_full_disk_access() -> bool {
    if !cfg!(target_os = "macos") {
        return true;
    }
    let home = PathBuf::from(lumen_home());
    let probes = [
        home.join("Library/Safari/Bookmarks.plist"),
        home.join("Library/Application Support/com.apple.TCC/TCC.db"),
        PathBuf::from("/Library/Application Support/com.apple.TCC/TCC.db"),
    ];
    for p in probes {
        match std::fs::File::open(&p) {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return false,
            Err(_) => continue,
        }
    }
    true
}

/// macOS lists an app under Full Disk Access only after it has tried to
/// read something that needs it, so try (and fail) first; then Lumen is in
/// the list, switched off, and the user only flips the switch. Then open the
/// pane: System Settings (macOS 13+) and System Preferences use different
/// addresses. v0.8.0 skipped the first step, so the list had no Lumen in it.
#[tauri::command(async)]
fn open_fda_settings() -> Res<()> {
    touch_protected();
    let pane = if macos_major() >= 13 {
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
    } else {
        "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
    };
    let ok = Command::new("open").arg(pane).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        // An address this macOS does not know: at least open Privacy & Security.
        Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security").spawn().map_err(err)?;
    }
    Ok(())
}

/// Read a few files only Full Disk Access may read. Each denied attempt is
/// what makes macOS add Lumen to the Full Disk Access list.
fn touch_protected() {
    let home = PathBuf::from(lumen_home());
    for p in [
        home.join("Library/Application Support/com.apple.TCC/TCC.db"),
        home.join("Library/Safari/Bookmarks.plist"),
        home.join("Library/Safari/History.db"),
        home.join("Library/Mail"),
        home.join("Library/Messages"),
        PathBuf::from("/Library/Application Support/com.apple.TCC/TCC.db"),
    ] {
        let _ = std::fs::File::open(&p);
        let _ = std::fs::read_dir(&p);
    }
}

fn macos_major() -> u32 {
    Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().split('.').next().and_then(|v| v.parse().ok()))
        .unwrap_or(13)
}

/// Lumen.app itself, selected in Finder, for dragging into the Full Disk
/// Access list when it does not show up there on its own.
#[tauri::command]
fn reveal_app() -> Res<()> {
    Command::new("open").arg("-R").arg(app_bundle().map_err(err)?).spawn().map_err(err)?;
    Ok(())
}

/// Lumen.log selected in Finder (~/Library/Logs/app.lumen.desktop), so a
/// bug report can attach it. Opens the folder when there is no log yet.
#[tauri::command]
fn show_log(app: AppHandle) -> Res<()> {
    let dir = app.path().app_log_dir().map_err(err)?;
    let file = dir.join("Lumen.log");
    if file.exists() {
        Command::new("open").arg("-R").arg(&file).spawn().map_err(err)?;
    } else {
        std::fs::create_dir_all(&dir).map_err(err)?;
        Command::new("open").arg(&dir).spawn().map_err(err)?;
    }
    Ok(())
}

fn app_bundle() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.ancestors().find(|p| p.extension().map(|e| e == "app").unwrap_or(false)).unwrap_or(&exe).to_path_buf())
}

/// Quit, then open Lumen again a second later. Not `app.restart()`: with one
/// instance enforced, the new process could start while this one still holds
/// the lock, see "already running" and exit, leaving no Lumen at all.
fn relaunch_soon(app: &AppHandle) {
    match app_bundle() {
        Ok(bundle) if bundle.extension().map(|e| e == "app").unwrap_or(false) => {
            let spawned = Command::new("/bin/sh")
                .args(["-c", "sleep 1; /usr/bin/open \"$0\""])
                .arg(&bundle)
                .spawn();
            if spawned.is_ok() {
                app.exit(0);
                return;
            }
            app.restart();
        }
        _ => app.restart(),
    }
}

/// Polled by the UI while the user is in System Settings.
#[tauri::command(async)]
fn full_disk_access() -> bool {
    has_full_disk_access()
}

/// Quit and start again, so a new grant applies to every part of Lumen.
#[tauri::command]
fn relaunch(app: AppHandle) {
    relaunch_soon(&app);
}

/// Every Finder action first checks the file is still there; a file that
/// moved while Lumen was closed is dropped from the index so it never shows
/// up again, and the UI is told why the action did nothing.
fn existing(state: &AppState, path: &str) -> Res<()> {
    if Path::new(path).exists() {
        Ok(())
    } else {
        let _ = state.index.remove_path(path);
        Err("moved".into())
    }
}

#[tauri::command]
fn open_path(state: State<AppState>, path: String) -> Res<()> {
    existing(&state, &path)?;
    Command::new("open").arg(&path).spawn().map_err(err)?;
    let _ = state.index.record_open(&path);
    Ok(())
}

#[tauri::command]
fn reveal(state: State<AppState>, path: String) -> Res<()> {
    existing(&state, &path)?;
    Command::new("open").args(["-R", &path]).spawn().map_err(err)?;
    let _ = state.index.record_open(&path);
    Ok(())
}

#[tauri::command]
fn quick_look(state: State<AppState>, path: String) -> Res<()> {
    existing(&state, &path)?;
    Command::new("qlmanage")
        .args(["-p", &path])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(err)?;
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let visible = w.is_visible().unwrap_or(false);
        let focused = w.is_focused().unwrap_or(false);
        if visible && focused {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
            let _ = w.emit("app://summon", ());
        }
    }
}

/// The menu bar icon: click to summon, menu for the rest.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let lang = first_preferred_language()
        .or_else(|| std::env::var("LANG").ok())
        .map(|l| menu_lang(&l))
        .unwrap_or(MenuLang::En);
    let t = |hans: &'static str, hant: &'static str, en: &'static str| match lang {
        MenuLang::Hans => hans,
        MenuLang::Hant => hant,
        MenuLang::En => en,
    };
    let show = MenuItem::with_id(app, "show", t("打开 Lumen", "開啟 Lumen", "Open Lumen"), true, None::<&str>)?;
    let quick = MenuItem::with_id(app, "quick", t("快速搜索…", "快速搜尋…", "Quick Search…"), true, None::<&str>)?;
    let rescan = MenuItem::with_id(app, "rescan", t("立即重新扫描", "立即重新掃描", "Rescan Now"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t("退出 Lumen", "結束 Lumen", "Quit Lumen"), true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(app, &[&quick, &show, &rescan, &PredefinedMenuItem::separator(app)?, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("lumen")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Lumen")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            // "Open Lumen" always opens: toggling hid a window already in front.
            "show" => show_main(app),
            "quick" => toggle_quick(app),
            "rescan" => {
                let state = app.state::<AppState>();
                let _ = run_scan(app, &state);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                summon(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MenuLang {
    Hans,
    Hant,
    En,
}

/// The menu follows the user's first preferred language only: an English
/// Mac with Chinese further down the list gets an English menu.
fn menu_lang(tag: &str) -> MenuLang {
    let t = tag.trim().to_ascii_lowercase().replace('_', "-");
    if !(t == "zh" || t.starts_with("zh-") || t.starts_with("zh.")) {
        return MenuLang::En;
    }
    if t.split(['-', '.', '@']).any(|p| matches!(p, "hant" | "tw" | "hk" | "mo")) {
        MenuLang::Hant
    } else {
        MenuLang::Hans
    }
}

/// First entry of `defaults read -g AppleLanguages`, which prints
/// `(\n    "zh-Hant-TW",\n    "en-US"\n)`.
fn first_preferred_language() -> Option<String> {
    let out = Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output().ok()?;
    first_language(&String::from_utf8_lossy(&out.stdout))
}

fn first_language(list: &str) -> Option<String> {
    list.split(|c: char| c == ',' || c == '\n' || c == '(' || c == ')')
        .map(|s| s.trim().trim_matches('"').trim())
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

fn main() {
    tauri::Builder::default()
        // One Lumen at a time: a second launch (say, a copy still on the
        // DMG next to the one in Applications) shows the running one's window
        // instead of opening the same index from two processes.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        // ~/Library/Logs/app.lumen.desktop/Lumen.log, for when something goes
        // wrong on a Mac we cannot see.
        .plugin(
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout))
                .level(log::LevelFilter::Info)
                .max_file_size(2_000_000)
                .target(tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: Some("Lumen".into()) }))
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    // Lumen registers exactly one shortcut: the summon key.
                    if event.state() == ShortcutState::Pressed {
                        summon(app);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                log::error!("panic: {info}");
                default_hook(info);
            }));
            log::info!("Lumen {} starting", app.package_info().version);
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let config_path = data.join("config.json");
            let config = Config::load(&config_path);
            use_ocr(config.ocr);
            use_semantic(config.semantic);
            pace::set_pause_on_battery(config.pause_on_battery);
            // A damaged index is set aside and rebuilt rather than failing to launch.
            // A Lumen that is still quitting may hold the file for a moment.
            let mut tries = 0;
            let (index, rebuilt) = loop {
                match Index::open_or_reset(data.join("index.db")) {
                    Ok(v) => break v,
                    Err(e) if tries < 3 => {
                        log::warn!("index not ready ({e:#}); retrying");
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    Err(e) => {
                        log::error!("cannot open the index: {e:#}");
                        return Err(e.into());
                    }
                }
            };
            log::info!("index open (rebuilt: {rebuilt})");
            let index = Arc::new(index);
            let had_index = rebuilt || index.status().map(|s| s.generation > 0).unwrap_or(false);

            app.manage(AppState {
                index,
                config: Mutex::new(config),
                config_path,
                scanning: Arc::new(AtomicBool::new(false)),
                cancel: Mutex::new(None),
                watcher: Mutex::new(None),
                reading: Arc::new(AtomicBool::new(false)),
                read_cancel: Mutex::new(None),
                dup_cancel: Mutex::new(None),
                embedding: Arc::new(AtomicBool::new(false)),
                rescan: Arc::new(AtomicBool::new(false)),
                reread: Arc::new(AtomicBool::new(false)),
                rebuilding: AtomicBool::new(false),
                rebuilt: AtomicBool::new(rebuilt),
            });

            // The full integrity check, off the launch path. A damaged index
            // is reported to the UI, which offers Rebuild.
            {
                let handle = app.handle().clone();
                let index = app.state::<AppState>().index.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(20));
                    pace::background_thread();
                    match index.check_integrity() {
                        Ok(true) => {}
                        Ok(false) => {
                            log::warn!("index failed its integrity check");
                            let _ = handle.emit("index://damaged", ());
                        }
                        Err(e) => log::info!("integrity check skipped: {e:#}"),
                    }
                });
            }

            // Tell the UI when background reading starts or stops holding
            // (power cable, Low Power Mode, Pause).
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let mut last = pace::hold();
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        let now = pace::hold();
                        if now != last {
                            last = now;
                            let _ = handle.emit("pace://hold", now);
                        }
                    }
                });
            }

            let hotkey = app.state::<AppState>().config.lock().unwrap().hotkey.clone();
            match hotkey.parse::<Shortcut>() {
                Ok(sc) => {
                    if let Err(e) = app.global_shortcut().register(sc) {
                        log::warn!("{hotkey} is taken by another app: {e}");
                    }
                }
                Err(_) => log::warn!("invalid hotkey in config: {hotkey}"),
            }
            let theme = app.state::<AppState>().config.lock().unwrap().theme.clone();
            apply_theme(app.handle(), &theme);
            // Started at login: stay in the menu bar until summoned.
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            if let Err(e) = build_tray(app) {
                log::warn!("menu bar icon unavailable: {e}");
            }

            // Quietly look for a new version a minute after launch; the UI
            // shows a banner, nothing installs without a click.
            if app.state::<AppState>().config.lock().unwrap().auto_update {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                    if let Ok(Some(info)) = tauri::async_runtime::block_on(check_update(handle.clone())) {
                        let _ = handle.emit("update-available", info);
                    }
                });
            }

            if had_index {
                // Returning user: watch live, and catch up on whatever changed
                // while Lumen was closed with a quiet incremental scan.
                let handle = app.handle().clone();
                restart_watcher(&handle);
                let state = handle.state::<AppState>();
                let _ = run_scan(&handle, &state);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps Lumen one keystroke away (⌥Space).
            match event {
                WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // The quick panel goes away the moment you click elsewhere.
                WindowEvent::Focused(false) if window.label() == "quick" => {
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            search,
            semantic,
            overview,
            recent,
            largest,
            forgotten,
            duplicates,
            cancel_duplicates,
            start_scan,
            cancel_scan,
            set_roots,
            set_excluded,
            set_excluded_names,
            set_paused,
            suggestions,
            recently_opened,
            rebuild_index,
            set_options,
            set_hotkey,
            trash_paths,
            open_fda_settings,
            reveal_app,
            show_log,
            full_disk_access,
            relaunch,
            quick_resize,
            collections,
            collection,
            timeline,
            month,
            save_share_card,
            check_update,
            install_update,
            quick_hide,
            show_in_main,
            take_link,
            open_path,
            reveal,
            quick_look
        ])
        .build(tauri::generate_context!())
        .expect("error while building Lumen")
        .run(|_app, _event| {
            // Clicking the Dock icon brings the hidden window back.
            #[cfg(target_os = "macos")]
            // Clicking the Dock icon (or opening Lumen again from Finder)
            // always brings the window up. It used to toggle, and since the
            // click focuses the app first, a window already open was hidden.
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_main(_app);
            }
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &_event {
                for u in urls {
                    open_link(_app, u);
                }
            }
        });
}

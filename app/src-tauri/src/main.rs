// Lumen — the macOS shell around lumen-core.
//
// Responsibilities: own the index and config, run scans off the main thread
// and stream progress to the UI, keep the FSEvents watcher alive, expose
// Finder actions (open, reveal, Quick Look) and the ⌥Space summon hotkey.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lumen_core::{
    scan, Config, DuplicateGroup, FileRecord, Index, IndexStatus, Overview, SearchRequest,
    SearchResponse, Watcher,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

struct AppState {
    index: Arc<Index>,
    config: Mutex<Config>,
    config_path: PathBuf,
    scanning: Arc<AtomicBool>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    watcher: Mutex<Option<Watcher>>,
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
}

#[tauri::command]
fn snapshot(state: State<AppState>) -> Res<Snapshot> {
    let status = state.index.status().map_err(err)?;
    Ok(Snapshot {
        first_run: status.generation == 0,
        status,
        config: state.config.lock().unwrap().clone(),
        scanning: state.scanning.load(Ordering::Relaxed),
        home: lumen_home(),
    })
}

fn lumen_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

#[tauri::command]
fn search(state: State<AppState>, req: SearchRequest) -> Res<SearchResponse> {
    state.index.search(&req).map_err(err)
}

#[tauri::command]
fn overview(state: State<AppState>) -> Res<Overview> {
    state.index.overview().map_err(err)
}

#[tauri::command]
fn recent(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.recent(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command]
fn largest(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.largest(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command]
fn forgotten(state: State<AppState>, limit: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.forgotten(limit.unwrap_or(40)).map_err(err)
}

#[tauri::command]
async fn duplicates(state: State<'_, AppState>) -> Res<Vec<DuplicateGroup>> {
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || index.duplicates(1024 * 1024, 40))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>) -> Res<()> {
    run_scan(&app, &state)
}

fn run_scan(app: &AppHandle, state: &AppState) -> Res<()> {
    if state.scanning.swap(true, Ordering::SeqCst) {
        return Ok(()); // already running; the UI is subscribed to its events
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *state.cancel.lock().unwrap() = Some(cancel.clone());
    let index = state.index.clone();
    let cfg = state.config.lock().unwrap().clone();
    let scanning = state.scanning.clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("lumen-scan".into())
        .spawn(move || {
            let emitter = app.clone();
            let result = scan(&index, &cfg, cancel, |p| {
                let _ = emitter.emit("scan://progress", p);
            });
            scanning.store(false, Ordering::SeqCst);
            match result {
                Ok(report) => {
                    let _ = app.emit("scan://done", &report);
                    restart_watcher(&app);
                }
                Err(e) => {
                    let _ = app.emit("scan://error", e.to_string());
                }
            }
        })
        .map_err(err)?;
    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    if let Some(c) = state.cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

fn restart_watcher(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let emitter = app.clone();
    let mut slot = state.watcher.lock().unwrap();
    *slot = None; // stop the old stream first
    match Watcher::start(&state.index, &cfg, move |batch| {
        let _ = emitter.emit("index://changed", batch);
    }) {
        Ok(w) => *slot = Some(w),
        Err(e) => log::warn!("watcher failed to start: {e}"),
    }
}

#[tauri::command]
fn set_roots(app: AppHandle, state: State<AppState>, roots: Vec<String>) -> Res<Config> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        cfg.roots = roots.into_iter().map(PathBuf::from).collect();
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    run_scan(&app, &state)?;
    Ok(cfg)
}

#[tauri::command]
fn set_excluded(app: AppHandle, state: State<AppState>, paths: Vec<String>) -> Res<Config> {
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        cfg.excluded_paths = paths.into_iter().map(PathBuf::from).collect();
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    run_scan(&app, &state)?;
    Ok(cfg)
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
    Ok(())
}

#[tauri::command]
fn reveal(state: State<AppState>, path: String) -> Res<()> {
    existing(&state, &path)?;
    Command::new("open").args(["-R", &path]).spawn().map_err(err)?;
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

fn main() {
    let summon = Shortcut::new(Some(Modifiers::ALT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed && shortcut == &summon {
                        toggle_main(app);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let config_path = data.join("config.json");
            let config = Config::load(&config_path);
            let index = Arc::new(Index::open(data.join("index.db"))?);
            let had_index = index.status().map(|s| s.generation > 0).unwrap_or(false);

            app.manage(AppState {
                index,
                config: Mutex::new(config),
                config_path,
                scanning: Arc::new(AtomicBool::new(false)),
                cancel: Mutex::new(None),
                watcher: Mutex::new(None),
            });

            if let Err(e) = app.global_shortcut().register(summon) {
                log::warn!("⌥Space is taken by another app: {e}");
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
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            search,
            overview,
            recent,
            largest,
            forgotten,
            duplicates,
            start_scan,
            cancel_scan,
            set_roots,
            set_excluded,
            open_path,
            reveal,
            quick_look
        ])
        .build(tauri::generate_context!())
        .expect("error while building Lumen")
        .run(|_app, _event| {
            // Clicking the Dock icon brings the hidden window back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                toggle_main(_app);
            }
        });
}

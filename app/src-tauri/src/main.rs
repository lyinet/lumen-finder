// Lumen — the macOS shell around lumen-core.
//
// Responsibilities: own the index and config, run scans and content reading
// off the main thread and stream progress to the UI, keep the FSEvents watcher
// alive, expose Finder actions (open, reveal, Quick Look, move to Trash), the
// menu bar icon and the configurable summon hotkey.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lumen_core::{
    index_content, scan, set_ocr_helper, CollectionStat, Config, DuplicateGroup, FileRecord, Index, IndexStatus, Overview,
    SearchRequest, SearchResponse, Watcher,
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

struct AppState {
    index: Arc<Index>,
    config: Mutex<Config>,
    config_path: PathBuf,
    scanning: Arc<AtomicBool>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    watcher: Mutex<Option<Watcher>>,
    reading: Arc<AtomicBool>,
    read_cancel: Mutex<Option<Arc<AtomicBool>>>,
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
        reading: state.reading.load(Ordering::Relaxed),
        full_disk_access: has_full_disk_access(),
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
                    if !report.cancelled && cfg.content_search {
                        run_content(&app);
                    }
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
    if let Some(c) = state.read_cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

/// Read the words of new and changed documents, after a scan.
fn run_content(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.reading.swap(true, Ordering::SeqCst) {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *state.read_cancel.lock().unwrap() = Some(cancel.clone());
    let index = state.index.clone();
    let reading = state.reading.clone();
    let app = app.clone();
    let _ = std::thread::Builder::new().name("lumen-content".into()).spawn(move || {
        let emitter = app.clone();
        let r = index_content(&index, cancel, |p| {
            let _ = emitter.emit("content://progress", p);
        });
        reading.store(false, Ordering::SeqCst);
        match r {
            Ok(report) => {
                let _ = app.emit("content://done", &report);
            }
            Err(e) => log::warn!("content indexing failed: {e}"),
        }
    });
}

fn restart_watcher(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let emitter = app.clone();
    let mut slot = state.watcher.lock().unwrap();
    *slot = None; // stop the old stream first
    match Watcher::start(&state.index, &cfg, move |batch| {
        let plugged = batch.volumes_changed;
        let _ = emitter.emit("index://changed", batch);
        if plugged {
            // A drive came or went: pick up its files, or mark them offline.
            let app = emitter.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let state = app.state::<AppState>();
                let _ = run_scan(&app, &state);
            });
        }
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

/// Smart collections and how many files each holds.
#[tauri::command]
fn collections(state: State<AppState>) -> Res<Vec<CollectionStat>> {
    state.index.collections().map_err(err)
}

#[tauri::command]
fn collection(state: State<AppState>, id: String, limit: Option<usize>, offset: Option<usize>) -> Res<Vec<FileRecord>> {
    state.index.collection(&id, limit.unwrap_or(200), offset.unwrap_or(0)).map_err(err)
}

/// The Vision OCR helper ships next to the app binary (Contents/MacOS).
fn use_ocr(on: bool) {
    let helper = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lumen-ocr")));
    set_ocr_helper(if on { helper } else { None });
}

/// Content search and external drives on or off.
#[tauri::command]
fn set_options(
    app: AppHandle,
    state: State<AppState>,
    content_search: Option<bool>,
    include_external: Option<bool>,
    ocr: Option<bool>,
    quick_panel: Option<bool>,
    launch_at_login: Option<bool>,
    theme: Option<String>,
) -> Res<Config> {
    if let Some(on) = launch_at_login {
        let al = app.autolaunch();
        if on { al.enable() } else { al.disable() }.map_err(err)?;
    }
    let cfg = {
        let mut cfg = state.config.lock().unwrap();
        if let Some(v) = content_search {
            cfg.content_search = v;
        }
        if let Some(v) = include_external {
            cfg.include_external = v;
        }
        if let Some(v) = ocr {
            cfg.ocr = v;
            use_ocr(v);
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
        cfg.save(&state.config_path).map_err(err)?;
        cfg.clone()
    };
    if theme.is_some() {
        apply_theme(&app, &cfg.theme);
    }
    // Only the options that change what is indexed need a scan.
    if content_search.is_some() || include_external.is_some() || ocr.is_some() {
        run_scan(&app, &state)?;
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
        let _ = w.emit("app://query", query);
    }
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
#[tauri::command]
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

#[tauri::command]
fn open_fda_settings() -> Res<()> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn()
        .map_err(err)?;
    Ok(())
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

/// The menu bar icon: click to summon, menu for the rest.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let zh = std::env::var("LANG").map(|l| l.starts_with("zh")).unwrap_or(false)
        || sys_locale_is_zh();
    let t = |zh_s: &'static str, en: &'static str| if zh { zh_s } else { en };
    let show = MenuItem::with_id(app, "show", t("打开 Lumen", "Open Lumen"), true, None::<&str>)?;
    let quick = MenuItem::with_id(app, "quick", t("快速搜索…", "Quick Search…"), true, None::<&str>)?;
    let rescan = MenuItem::with_id(app, "rescan", t("立即重新扫描", "Rescan Now"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t("退出 Lumen", "Quit Lumen"), true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(app, &[&quick, &show, &rescan, &PredefinedMenuItem::separator(app)?, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("lumen")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Lumen")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => toggle_main(app),
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

fn sys_locale_is_zh() -> bool {
    Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("zh"))
        .unwrap_or(false)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let config_path = data.join("config.json");
            let config = Config::load(&config_path);
            use_ocr(config.ocr);
            let index = Arc::new(Index::open(data.join("index.db"))?);
            let had_index = index.status().map(|s| s.generation > 0).unwrap_or(false);

            app.manage(AppState {
                index,
                config: Mutex::new(config),
                config_path,
                scanning: Arc::new(AtomicBool::new(false)),
                cancel: Mutex::new(None),
                watcher: Mutex::new(None),
                reading: Arc::new(AtomicBool::new(false)),
                read_cancel: Mutex::new(None),
            });

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
            overview,
            recent,
            largest,
            forgotten,
            duplicates,
            start_scan,
            cancel_scan,
            set_roots,
            set_excluded,
            set_options,
            set_hotkey,
            trash_paths,
            open_fda_settings,
            quick_resize,
            collections,
            collection,
            quick_hide,
            show_in_main,
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

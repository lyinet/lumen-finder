//! Cloud sync folders (v0.9): ~/Library/CloudStorage/* is indexed while the
//! rest of ~/Library stays skipped, every file carries its provider, `cloud:`
//! narrows a search to one service, and files not on this Mac are never
//! opened. Its own test binary: it points HOME at a scratch folder, which is
//! process-wide, so the tests here take turns.

use lumen_core::config::{cloud_storage, cloud_storage_roots, provider_of};
use lumen_core::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

static TURN: Mutex<()> = Mutex::new(());

/// A scratch home folder, set as HOME before anything reads it.
fn home() -> &'static Path {
    static HOME: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &HOME
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            fs::create_dir_all(&home).unwrap();
            std::env::set_var("HOME", &home);
            (dir, home)
        })
        .1
}

/// An empty home (the scratch folder is shared by the tests in turn).
fn fresh() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
    let guard = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = home().to_path_buf();
    for e in fs::read_dir(&home).unwrap().flatten() {
        let _ = fs::remove_dir_all(e.path()).or_else(|_| fs::remove_file(e.path()));
    }
    (guard, home)
}

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn names(index: &Index, query: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: query.into(), limit: 100, ..Default::default() }).unwrap();
    let mut v: Vec<String> = r.hits.into_iter().map(|h| h.name).collect();
    v.sort();
    v
}

fn cfg(home: &Path) -> Config {
    Config { roots: vec![home.to_path_buf()], include_external: false, ..Config::default() }
}

#[test]
fn cloud_storage_folders_are_roots_inside_a_skipped_library() {
    let (_g, home) = fresh();
    let storage = home.join("Library/CloudStorage");
    fs::create_dir_all(storage.join("Dropbox")).unwrap();
    fs::create_dir_all(storage.join("OneDrive-Personal")).unwrap();
    fs::create_dir_all(storage.join(".Trash")).unwrap();
    touch(&storage.join("stray.pdf"), b"not a sync folder");
    assert_eq!(cloud_storage(), Some(storage.clone()));
    assert_eq!(cloud_storage_roots(&storage), vec![storage.join("Dropbox"), storage.join("OneDrive-Personal")]);

    let roots = cfg(&home).effective_roots();
    assert!(roots.contains(&storage.join("Dropbox")), "{roots:?}");
    assert!(roots.contains(&storage.join("OneDrive-Personal")));
    let filter = cfg(&home).filter();
    assert!(filter.allow_dir(&storage.join("Dropbox/工作")));
    assert!(!filter.allow_dir(&home.join("Library/Caches")));
    assert!(filter.allow_path_chain(&storage.join("Dropbox/工作/a.pdf"), &roots));

    let off = Config { include_cloud_storage: false, ..cfg(&home) };
    assert!(!off.effective_roots().iter().any(|r| r.starts_with(&storage)));
    assert!(!off.filter().allow_dir(&storage.join("Dropbox")));
}

#[test]
fn files_carry_their_provider_and_cloud_filters_narrow_by_it() {
    let (_g, home) = fresh();
    let storage = home.join("Library/CloudStorage");
    touch(&storage.join("Dropbox/求职/Résumé 2025.pdf"), b"pdf");
    touch(&storage.join("OneDrive-Personal/工作/合同模板.docx"), b"docx");
    touch(&storage.join("GoogleDrive-me@example.com/My Drive/roadmap.key"), b"key");
    touch(&storage.join("pCloud Drive/photos/trip.jpg"), b"jpg");
    touch(&home.join("Nutstore Files/我的坚果云/周报.md"), "本周完成".as_bytes());
    touch(&home.join("BaiduNetdisk/资料/课件.pdf"), b"pdf");
    touch(&home.join("Documents/plain.txt"), b"plain");
    touch(&home.join("Library/Caches/skipped.pdf"), b"x");
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let r = scan(&index, &cfg(&home), Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.files, 7, "every sync folder, nothing else from ~/Library");

    let all = index.search(&SearchRequest { query: String::new(), limit: 100, ..Default::default() }).unwrap();
    let prov = |name: &str| {
        all.hits.iter().find(|h| h.name == name).unwrap_or_else(|| panic!("{name}")).provider.clone().map(|p| (p.id, p.name))
    };
    let p = |id: &str, name: &str| Some((id.to_string(), name.to_string()));
    assert_eq!(prov("Résumé 2025.pdf"), p("dropbox", "Dropbox"));
    assert_eq!(prov("合同模板.docx"), p("onedrive", "OneDrive"));
    assert_eq!(prov("roadmap.key"), p("gdrive", "Google Drive"));
    assert_eq!(prov("trip.jpg"), p("pclouddrive", "pCloud Drive"));
    assert_eq!(prov("周报.md"), p("nutstore", "Nutstore"));
    assert_eq!(prov("课件.pdf"), p("baidu", "Baidu Netdisk"));
    assert_eq!(prov("plain.txt"), None);
    // Serialized for the app only when there is one.
    let json = serde_json::to_value(&all.hits).unwrap();
    assert!(json.as_array().unwrap().iter().any(|h| h["provider"]["id"] == "dropbox"));
    assert!(json.as_array().unwrap().iter().any(|h| h.get("provider").is_none()));

    assert_eq!(names(&index, "cloud:dropbox"), vec!["Résumé 2025.pdf"]);
    assert_eq!(names(&index, "cloud:OneDrive 合同"), vec!["合同模板.docx"]);
    assert_eq!(names(&index, "cloud:google"), vec!["roadmap.key"]);
    assert_eq!(names(&index, "云盘:坚果云"), vec!["周报.md"]);
    assert_eq!(names(&index, "cloud:百度网盘"), vec!["课件.pdf"]);
    assert_eq!(names(&index, "cloud:pcloud"), vec!["trip.jpg"]);
    assert_eq!(names(&index, "cloud:any").len(), 6);
    assert_eq!(names(&index, "cloud:none"), vec!["plain.txt"]);
    assert_eq!(names(&index, "cloud:dropbox cloud:onedrive"), Vec::<String>::new(), "filters combine with AND");
    assert!(names(&index, "cloud:nosuchservice").is_empty());
    // In ranked (word) searches too, and the chip counts follow the filter.
    let r = index.search(&SearchRequest { query: "cloud:any 2025".into(), ..Default::default() }).unwrap();
    assert_eq!(r.total, 1);
}

#[test]
fn provider_of_paths() {
    let home = Path::new("/Users/a");
    let id = |p: &str| provider_of(p, Some(home)).map(|p| p.id);
    assert_eq!(id("/Users/a/Library/Mobile Documents/com~apple~CloudDocs/x.pdf").as_deref(), Some("icloud"));
    assert_eq!(id("/Users/a/Library/CloudStorage/Dropbox/x.pdf").as_deref(), Some("dropbox"));
    assert_eq!(id("/Users/a/Library/CloudStorage/Dropbox-Team/x.pdf").as_deref(), Some("dropbox"));
    assert_eq!(id("/Users/a/Library/CloudStorage/OneDrive-SharedLibraries-Acme/x.pdf").as_deref(), Some("onedrive"));
    assert_eq!(id("/Users/a/Library/CloudStorage/Box-Box/x.pdf").as_deref(), Some("box"));
    assert_eq!(id("/Users/a/Library/CloudStorage/BoxCryptor/x.pdf").as_deref(), Some("boxcryptor"));
    assert_eq!(id("/Users/a/Library/CloudStorage/Dropbox").as_deref(), None);
    assert_eq!(id("/Users/a/Dropbox/x.pdf").as_deref(), Some("dropbox"));
    assert_eq!(id("/Users/a/Dropbox").as_deref(), None);
    assert_eq!(id("/Users/a/Documents/Dropbox/x.pdf").as_deref(), None, "only the client's own folder");
    assert_eq!(id("/Users/ab/Dropbox/x.pdf").as_deref(), None);
    assert_eq!(provider_of("/Users/a/Dropbox/x.pdf", None), None);
}

#[test]
fn files_not_on_this_mac_are_never_opened() {
    let (_g, home) = fresh();
    let dropbox = home.join("Library/CloudStorage/Dropbox");
    // Placeholders stand in for dataless files here (SF_DATALESS is macOS
    // only); both are "not on this Mac" to every reader.
    touch(&dropbox.join(".季度预算.md.icloud"), b"bplist00 placeholder");
    touch(&dropbox.join("本地笔记.md"), "预算已经批准".as_bytes());
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    scan(&index, &cfg(&home), Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let hit = index.search(&SearchRequest { query: "季度预算".into(), ..Default::default() }).unwrap().hits.remove(0);
    assert!(hit.cloud);
    assert_eq!(hit.provider.map(|p| p.id).as_deref(), Some("dropbox"));
    assert!(!lumen_core::index::is_local(&dropbox.join(".季度预算.md.icloud")));
    assert!(lumen_core::index::is_local(&dropbox.join("本地笔记.md")));
    assert!(!lumen_core::index::is_local(&dropbox.join("gone.md")));
    assert!(content::extract(&dropbox.join(".季度预算.md.icloud"), "md").is_err(), "extract refuses");

    let r = index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.read, 1, "only the local note is opened");
    assert!(index.duplicates(1, 10).unwrap().is_empty());
}

#[test]
fn a_file_evicted_after_the_scan_is_not_read() {
    // The index still says "on this Mac" (cloud = 0), but the file has gone
    // back to the cloud since: the reader looks again before opening it.
    let (_g, home) = fresh();
    let dropbox = home.join("Library/CloudStorage/Dropbox");
    touch(&dropbox.join("notes.md"), b"budget approved");
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let c = Config { content_search: false, ..cfg(&home) };
    scan(&index, &c, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    // "Evicted": what is left on disk is a placeholder, the row is the same.
    fs::rename(dropbox.join("notes.md"), dropbox.join(".notes.md.icloud")).unwrap();
    let r = index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.read, 0);
    assert_eq!(index.status().unwrap().content_files, 0);
}

#[test]
fn the_watcher_follows_cloud_storage_roots() {
    let (_g, home) = fresh();
    let dropbox = home.join("Library/CloudStorage/Dropbox");
    fs::create_dir_all(dropbox.join("工作")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let c = Config { content_search: false, ..cfg(&home) };
    scan(&index, &c, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let watcher = Watcher::start(&index, &c, |_| {}).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    touch(&dropbox.join("工作/报价单.pdf"), b"quote");
    touch(&home.join("Library/Caches/noise.pdf"), b"x");
    let wait = |want: usize| {
        let t = Instant::now();
        while names(&index, "cloud:dropbox").len() != want {
            assert!(t.elapsed() < Duration::from_secs(15), "watcher never caught up");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    wait(1);
    assert_eq!(names(&index, "报价"), vec!["报价单.pdf"]);
    assert!(names(&index, "noise").is_empty(), "the rest of ~/Library stays out");
    fs::remove_file(dropbox.join("工作/报价单.pdf")).unwrap();
    wait(0);
    drop(watcher);
}

//! Cloud sync folders as someone actually has them (v0.9 review 2): a home
//! with ~/Library/CloudStorage/{Dropbox, OneDrive-Personal,
//! GoogleDrive-a@b.com/My Drive}. Its own test binary: it points HOME at a
//! scratch folder, which is process-wide, so the tests here take turns.

use lumen_core::index::NewFile;
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

/// A home with the three usual sync folders and nothing else.
fn fresh() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
    let guard = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = home().to_path_buf();
    for e in fs::read_dir(&home).unwrap().flatten() {
        let _ = fs::remove_dir_all(e.path()).or_else(|_| fs::remove_file(e.path()));
    }
    for d in ["Dropbox", "OneDrive-Personal", "GoogleDrive-a@b.com/My Drive"] {
        fs::create_dir_all(storage(&home).join(d)).unwrap();
    }
    (guard, home)
}

fn storage(home: &Path) -> PathBuf {
    home.join("Library/CloudStorage")
}

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn cfg(roots: Vec<PathBuf>) -> Config {
    Config { roots, include_external: false, content_search: false, ..Config::default() }
}

fn search(index: &Index, query: &str) -> Vec<FileRecord> {
    index.search(&SearchRequest { query: query.into(), limit: 500, ..Default::default() }).unwrap().hits
}

fn names(index: &Index, query: &str) -> Vec<String> {
    let mut v: Vec<String> = search(index, query).into_iter().map(|h| h.name).collect();
    v.sort();
    v
}

/// A file as the scanner stores it, `cloud` meaning dataless: a File
/// Provider file not on this Mac keeps its full size (SF_DATALESS, macOS
/// only, so it is written straight into the index here).
fn file(path: &Path, size: i64, years_old: i64, cloud: bool) -> NewFile {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let ext = path.extension().unwrap().to_string_lossy().into_owned();
    let mtime = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - years_old * 366 * 86_400;
    NewFile {
        path: path.to_string_lossy().into_owned(),
        name_py: name.to_lowercase(),
        name,
        parent: path.parent().unwrap().to_string_lossy().into_owned(),
        kind: Kind::from_ext(&ext).unwrap_or(Kind::Document),
        ext,
        size,
        mtime,
        ctime: mtime,
        cloud,
    }
}

/// "Large files", "Forgotten" and the overview's forgotten count are about
/// space on this Mac and what to clear out. An online-only Dropbox or
/// OneDrive file takes no space here, yet keeps its full size, so it topped
/// both lists, and moving it to the Trash deletes it from the service.
#[test]
fn space_views_leave_out_files_only_in_the_cloud() {
    let (_g, home) = fresh();
    let s = storage(&home);
    const MB: i64 = 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    // An iCloud-style placeholder, found by a real scan.
    touch(&s.join("Dropbox/.报告.pdf.icloud"), b"bplist00 placeholder");
    scan(&index, &cfg(vec![s.join("Dropbox")]), Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index
        .insert_files(&[
            file(&s.join("Dropbox/录像/婚礼.mov"), 4000 * MB, 3, true),
            file(&s.join("OneDrive-Personal/Backups/old-laptop.zip"), 900 * MB, 3, true),
            file(&s.join("GoogleDrive-a@b.com/My Drive/raw.mov"), 700 * MB, 0, true),
            file(&s.join("Dropbox/本地/kept.mov"), 300 * MB, 3, false),
            file(&home.join("Movies/holiday.mov"), 200 * MB, 3, false),
            file(&home.join("Documents/new.zip"), 100 * MB, 0, false),
        ])
        .unwrap();
    let r = search(&index, "报告");
    assert!(r.len() == 1 && r[0].cloud && r[0].size == 0);

    let large: Vec<String> = index.largest(40).unwrap().into_iter().map(|f| f.name).collect();
    assert_eq!(large, vec!["kept.mov", "holiday.mov", "new.zip"], "biggest files on this Mac only");
    let forgotten: Vec<String> = index.forgotten(40).unwrap().into_iter().map(|f| f.name).collect();
    assert_eq!(forgotten, vec!["kept.mov", "holiday.mov"]);
    let o = index.overview().unwrap();
    assert_eq!((o.forgotten_files, o.forgotten_bytes), (2, 500 * MB));

    // Totals and the timeline still count every file, wherever its bytes are.
    let total: i64 = o.kinds.iter().map(|k| k.files).sum();
    assert_eq!(total, 7);
    assert_eq!(o.kinds.iter().map(|k| k.bytes).sum::<i64>(), 6200 * MB);
    let months: i64 = index.timeline(1000).unwrap().iter().map(|m| m.files).sum();
    assert_eq!(months, 7);
    // Nor do they leave search.
    assert_eq!(names(&index, "cloud:dropbox").len(), 3);
}

/// Every provider id a result carries finds that result as `cloud:<id>`.
/// For services Lumen does not know, the id is the folder's name without
/// spaces and dots ("pCloud Drive" → `pclouddrive`), and `cloud:pclouddrive`
/// used to find nothing: the filter only matched the folder name as typed.
#[test]
fn every_provider_id_works_as_a_cloud_filter() {
    let (_g, home) = fresh();
    let s = storage(&home);
    touch(&s.join("Dropbox/a.pdf"), b"a");
    touch(&s.join("OneDrive-Personal/b.pdf"), b"b");
    touch(&s.join("GoogleDrive-a@b.com/My Drive/c.pdf"), b"c");
    touch(&s.join("pCloud Drive/d.pdf"), b"d");
    touch(&s.join("Synology.Drive-home/e.pdf"), b"e");
    touch(&s.join("Tresorit_Sync/f.pdf"), b"f");
    touch(&s.join("坚果云/g.pdf"), b"g");
    touch(&home.join("Library/Mobile Documents/com~apple~CloudDocs/h.pdf"), b"h");
    touch(&home.join("Documents/plain.pdf"), b"plain");
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    scan(&index, &cfg(vec![home.clone()]), Arc::new(AtomicBool::new(false)), |_| {}).unwrap();

    let hits = search(&index, "pdf");
    assert_eq!(hits.len(), 9, "{:?}", hits.iter().map(|h| &h.path).collect::<Vec<_>>());
    let mut ids = Vec::new();
    for h in hits.iter().filter(|h| h.provider.is_some()) {
        let id = h.provider.as_ref().unwrap().id.clone();
        assert!(names(&index, &format!("cloud:{id}")).contains(&h.name), "cloud:{id} misses {}", h.path);
        ids.push(id);
    }
    ids.sort();
    assert_eq!(ids, ["dropbox", "gdrive", "icloud", "nutstore", "onedrive", "pclouddrive", "synologydrive", "tresoritsync"]);
    assert_eq!(names(&index, "cloud:pclouddrive"), vec!["d.pdf"], "one service, not its neighbours");
    assert_eq!(names(&index, "cloud:pcloud"), vec!["d.pdf"], "the start of the folder name still works");
    assert_eq!(names(&index, "cloud:tresorit_sync"), vec!["f.pdf"]);
    assert_eq!(names(&index, "cloud:none"), vec!["plain.pdf"]);
    // Nothing to match by: no service, rather than every one.
    for q in ["cloud:%", "cloud:.", "cloud:_", "cloud:..", "cloud:-"] {
        assert!(names(&index, q).is_empty(), "{q}");
    }
}

/// Wait for a batch asking for a rescan, ignoring any others.
fn wait_rescan(rx: &crossbeam_channel::Receiver<ChangeBatch>) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
            if b.volumes_changed {
                return true;
            }
        }
    }
    false
}

fn watch(index: &Index, c: &Config) -> (Watcher, crossbeam_channel::Receiver<ChangeBatch>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let w = Watcher::start(index, c, move |b| {
        let _ = tx.send(b);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    (w, rx)
}

/// Signing in to a service while Lumen runs adds its folder to
/// ~/Library/CloudStorage: a new root, like a drive plugged in, so the app
/// is asked to rescan. Its files were not picked up until the next launch.
#[test]
fn a_sync_folder_added_while_running_asks_for_a_rescan() {
    let (_g, home) = fresh();
    let s = storage(&home);
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let c = cfg(vec![home.clone()]);
    scan(&index, &c, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let (w, rx) = watch(&index, &c);

    fs::create_dir(s.join("Box-Box")).unwrap();
    assert!(wait_rescan(&rx), "a new sync folder asks for a rescan");
    // The rescan the app then runs picks it up.
    touch(&s.join("Box-Box/plan.pdf"), b"plan");
    drop(w);
    scan(&index, &c, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(names(&index, "cloud:box"), vec!["plan.pdf"]);

    // Signing out removes it again: rescanned, its files leave.
    let (w, rx) = watch(&index, &c);
    fs::remove_dir_all(s.join("Box-Box")).unwrap();
    assert!(wait_rescan(&rx), "a sync folder gone asks for a rescan");
    drop(w);

    // Hidden folders (the providers' own) and excluded ones change nothing,
    // nor does a file next to the sync folders.
    let c = Config { excluded_paths: vec![s.join("Egnyte")], ..cfg(vec![home.clone()]) };
    let (w, rx) = watch(&index, &c);
    fs::create_dir(s.join(".Trash")).unwrap();
    fs::create_dir(s.join("Egnyte")).unwrap();
    touch(&s.join("notes.txt"), b"x");
    assert!(!wait_rescan(&rx), "nothing that adds a root");
    drop(w);
}

/// Only chosen folders scanned (home is not a root): nothing watched
/// ~/Library/CloudStorage itself, so a new sync folder went unseen even
/// with the fix above.
#[test]
fn a_sync_folder_added_is_seen_without_the_home_root() {
    let (_g, home) = fresh();
    let s = storage(&home);
    fs::create_dir_all(home.join("Documents")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let c = cfg(vec![home.join("Documents")]);
    assert!(c.effective_roots().contains(&s.join("Dropbox")));
    scan(&index, &c, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let (w, rx) = watch(&index, &c);
    fs::create_dir(s.join("pCloud Drive")).unwrap();
    assert!(wait_rescan(&rx), "seen without a root above it");
    drop(w);

    // Sync folders off in Settings: nothing to pick up.
    let c = Config { include_cloud_storage: false, ..cfg(vec![home.join("Documents")]) };
    let (w, rx) = watch(&index, &c);
    fs::create_dir(s.join("MEGA")).unwrap();
    assert!(!wait_rescan(&rx));
    drop(w);
}

//! Full and incremental scans. Every scan is a new "generation": files seen
//! get stamped with it, files whose size or mtime moved are marked changed,
//! and rows not seen at the end are files that left the disk.

use crate::config::{Config, PathFilter};
use crate::index::{get_meta_i64, now, remove_tree, set_meta, Index, NewFile, UPSERT};
use anyhow::Result;
use crossbeam_channel::bounded;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BATCH: usize = 4_000;

#[derive(Debug, Clone, Serialize)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub dirs: u64,
    pub files_found: u64,
    pub bytes_found: u64,
    pub elapsed_ms: u64,
    /// The folder being walked right now, for the live "sonar" readout.
    pub current: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScanPhase {
    Walking,
    Finishing,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub generation: i64,
    pub full: bool,
    pub cancelled: bool,
    pub files: i64,
    pub bytes: i64,
    pub added: i64,
    pub changed: i64,
    pub removed: i64,
    pub dirs: u64,
    pub elapsed_ms: u64,
}

#[derive(Default)]
struct Counters {
    dirs: AtomicU64,
    files: AtomicU64,
    bytes: AtomicU64,
    current: Mutex<String>,
    /// Folders the walk could not read (permission denied): their files are
    /// unseen, not gone.
    denied: Mutex<Vec<PathBuf>>,
}

/// Scan every root in `cfg`. Safe to call on an existing index: that is the
/// incremental path, and it only rewrites rows that changed.
pub fn scan(
    index: &Index,
    cfg: &Config,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(&ScanProgress),
) -> Result<ScanReport> {
    let started = Instant::now();
    let roots = cfg.effective_roots();
    let filter = cfg.filter();
    let mut writer = index.writer()?;
    precompose_names_once(&mut writer)?;
    let gen = get_meta_i64(&writer, "generation")?.unwrap_or(0) + 1;
    let full = gen == 1;

    let counters = Arc::new(Counters::default());
    let (tx, rx) = bounded::<NewFile>(BATCH * 4);

    // Walkers: one parallel walk across all roots.
    let walk = {
        let counters = counters.clone();
        let cancel = cancel.clone();
        let filter = filter.clone();
        let roots = roots.clone();
        std::thread::spawn(move || walk_parallel(&roots, &filter, &counters, &cancel, tx))
    };

    // Writer: this thread, batching inserts, reporting progress as it goes.
    let mut last_tick = Instant::now() - Duration::from_secs(1);
    let mut batch = Vec::with_capacity(BATCH);
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(f) => {
                batch.push(f);
                if batch.len() >= BATCH {
                    write_batch(&mut writer, &batch, gen)?;
                    batch.clear();
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        if last_tick.elapsed() >= Duration::from_millis(100) {
            last_tick = Instant::now();
            on_progress(&snapshot(&counters, ScanPhase::Walking, started));
        }
    }
    write_batch(&mut writer, &batch, gen)?;
    let _ = walk.join();

    let cancelled = cancel.load(Ordering::Relaxed);
    on_progress(&snapshot(&counters, ScanPhase::Finishing, started));

    let mut removed = 0i64;
    if !cancelled {
        // Files not stamped this generation are gone, except under roots that
        // are configured but not reachable right now (an unplugged drive, a
        // link to one): those are kept and shown as offline. A root that was
        // deleted is not offline: before, its files stayed in search forever.
        let mut offline: Vec<PathBuf> = cfg.roots.iter().filter(|r| root_offline(r)).cloned().collect();
        // Folders the walk was not allowed into keep what they had.
        offline.extend(counters.denied.lock().map(|d| d.clone()).unwrap_or_default());
        let tx = writer.transaction()?;
        let has_volume_rows: bool = tx
            .query_row("SELECT 1 FROM files WHERE seen_gen < ?1 AND path LIKE '/Volumes/%' LIMIT 1", [gen], |_| Ok(true))
            .unwrap_or(false);
        if offline.is_empty() && !has_volume_rows {
            removed = tx.execute("DELETE FROM files WHERE seen_gen < ?1", [gen])? as i64;
        } else {
            let mut stmt = tx.prepare("SELECT path FROM files WHERE seen_gen < ?1")?;
            let stale: Vec<String> = stmt
                .query_map([gen], |r| r.get(0))?
                .filter_map(|r| r.ok())
                .filter(|p: &String| {
                    let p = Path::new(p);
                    !offline.iter().any(|o| p.starts_with(o)) && !crate::config::volume_offline(p)
                })
                .collect();
            drop(stmt);
            let mut del = tx.prepare_cached("DELETE FROM files WHERE path = ?1")?;
            for p in stale {
                removed += del.execute([p])? as i64;
            }
        }
        set_meta(&tx, "generation", gen)?;
        set_meta(&tx, "last_scan_at", now())?;
        set_meta(&tx, "last_scan_ms", started.elapsed().as_millis())?;
        tx.commit()?;
        writer.execute_batch("PRAGMA optimize;")?;
    }

    let (files, bytes, added, changed): (i64, i64, i64, i64) = writer.query_row(
        "SELECT count(*), coalesce(sum(size),0),
                coalesce(sum(first_gen = ?1), 0),
                coalesce(sum(changed_gen = ?1 AND first_gen < ?1), 0)
         FROM files",
        [gen],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;

    let phase = if cancelled { ScanPhase::Cancelled } else { ScanPhase::Done };
    on_progress(&snapshot(&counters, phase, started));
    Ok(ScanReport {
        generation: gen,
        full,
        cancelled,
        files,
        bytes,
        added,
        changed,
        removed,
        dirs: counters.dirs.load(Ordering::Relaxed),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Names and folders are stored precomposed since 0.8.2 (see `crate::nfc`).
/// A rescan does not rewrite the name of a row it already has, so rows from
/// before keep a decomposed name, which a typed query never finds: fix them
/// once, on the scan's own (background) connection.
fn precompose_names_once(conn: &mut rusqlite::Connection) -> Result<()> {
    if get_meta_i64(conn, "nfc_v1")?.is_some() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut read = tx.prepare("SELECT id, name, parent FROM files")?;
        let fix: Vec<(i64, String, String)> = read
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, name, parent): (i64, String, String)| {
                let (n, p) = (crate::nfc(&name), crate::nfc(&parent));
                (n != name.as_str() || p != parent.as_str()).then(|| (id, n.into_owned(), p.into_owned()))
            })
            .collect();
        let mut put = tx.prepare("UPDATE files SET name = ?2, name_py = ?3, parent = ?4 WHERE id = ?1")?;
        for (id, name, parent) in fix {
            put.execute(rusqlite::params![id, name, crate::pinyin_key(&name), parent])?;
        }
    }
    set_meta(&tx, "nfc_v1", 1)?;
    tx.commit()?;
    Ok(())
}

/// A configured root that is missing but may come back: on a drive that is
/// not mounted, or still present as an entry (a link to an unplugged drive,
/// a folder Lumen may not look at). Only a root that no longer exists at all
/// on a mounted disk is gone.
fn root_offline(root: &Path) -> bool {
    if root.is_dir() {
        return false;
    }
    if crate::config::volume_offline(root) {
        return true;
    }
    match std::fs::symlink_metadata(root) {
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        // Replaced by a plain file: the folder is gone, not unplugged (its
        // files used to be kept as "offline" forever).
        Ok(m) => !m.is_file(),
    }
}

/// The folder behind a walk error, when the error is "not allowed to read it".
fn denied_dir(e: &ignore::Error) -> Option<PathBuf> {
    let mut path = None;
    let mut cur = e;
    loop {
        match cur {
            ignore::Error::WithPath { path: p, err } => {
                path.get_or_insert_with(|| p.clone());
                cur = err;
            }
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => cur = err,
            ignore::Error::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => return path,
            _ => return None,
        }
    }
}

/// The error is "permission denied", with or without a path attached.
fn denied_io(e: &ignore::Error) -> bool {
    match e {
        ignore::Error::WithPath { err, .. } | ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => denied_io(err),
        ignore::Error::Io(io) => io.kind() == std::io::ErrorKind::PermissionDenied,
        _ => false,
    }
}

fn snapshot(c: &Counters, phase: ScanPhase, started: Instant) -> ScanProgress {
    ScanProgress {
        phase,
        dirs: c.dirs.load(Ordering::Relaxed),
        files_found: c.files.load(Ordering::Relaxed),
        bytes_found: c.bytes.load(Ordering::Relaxed),
        elapsed_ms: started.elapsed().as_millis() as u64,
        current: c.current.lock().map(|s| s.clone()).unwrap_or_default(),
    }
}

fn write_batch(conn: &mut rusqlite::Connection, batch: &[NewFile], gen: i64) -> Result<()> {
    write_rows(conn, batch, gen, false)
}

/// `touched`: also remember the rows for the watcher's reading (see
/// `TOUCHED`), in the same transaction: one commit per batch, not one per
/// file.
fn write_rows(conn: &mut rusqlite::Connection, batch: &[NewFile], gen: i64, touched: bool) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    if touched {
        conn.execute_batch(TOUCHED)?;
    }
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(UPSERT)?;
        for f in batch {
            f.upsert(&mut stmt, gen)?;
        }
        if touched {
            let mut mark = tx.prepare_cached(
                "INSERT OR IGNORE INTO temp.lumen_touched(id) SELECT id FROM files WHERE path = ?1",
            )?;
            for f in batch {
                mark.execute([&f.path])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn walk_parallel(
    roots: &[PathBuf],
    filter: &PathFilter,
    counters: &Arc<Counters>,
    cancel: &Arc<AtomicBool>,
    tx: crossbeam_channel::Sender<NewFile>,
) {
    let Some(first) = roots.first() else { return };
    let mut builder = ignore::WalkBuilder::new(first);
    for r in &roots[1..] {
        builder.add(r);
    }
    let dir_filter = filter.clone();
    builder
        .standard_filters(false) // Lumen's own rules, not .gitignore's
        .follow_links(false)
        .same_file_system(false)
        .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12))
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            match e.file_type() {
                Some(t) if t.is_dir() => dir_filter.allow_dir(e.path()),
                _ => true,
            }
        });

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let filter = filter.clone();
        let counters = counters.clone();
        let cancel = cancel.clone();
        let mut local_dirs = 0u64;
        Box::new(move |res| {
            use ignore::WalkState;
            if cancel.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match res {
                Ok(e) => e,
                Err(e) => {
                    if let Some(dir) = denied_dir(&e) {
                        if let Ok(mut d) = counters.denied.lock() {
                            d.push(dir);
                        }
                    }
                    return WalkState::Continue;
                }
            };
            let Some(ft) = entry.file_type() else { return WalkState::Continue };
            if ft.is_dir() {
                counters.dirs.fetch_add(1, Ordering::Relaxed);
                local_dirs += 1;
                if local_dirs % 64 == 1 {
                    if let Ok(mut cur) = counters.current.try_lock() {
                        *cur = entry.path().to_string_lossy().into_owned();
                    }
                }
                return WalkState::Continue;
            }
            if !ft.is_file() {
                return WalkState::Continue;
            }
            let path = entry.path();
            let Some(kind) = filter.classify_file(path) else { return WalkState::Continue };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    // Listed but not allowed to look at (a folder that can be
                    // read but not entered): unseen, not deleted.
                    if let Some(p) = denied_dir(&e).or_else(|| denied_io(&e).then(|| path.to_path_buf())) {
                        if let Ok(mut d) = counters.denied.lock() {
                            d.push(p);
                        }
                    }
                    return WalkState::Continue;
                }
            };
            if !filter.keep_size(path, &meta) {
                return WalkState::Continue;
            }
            if let Some(f) = NewFile::from_path(path, kind, &meta) {
                counters.files.fetch_add(1, Ordering::Relaxed);
                counters.bytes.fetch_add(meta.len(), Ordering::Relaxed);
                if tx.send(f).is_err() {
                    return WalkState::Quit;
                }
            }
            WalkState::Continue
        })
    });
}

/// Index one folder tree in place (used by the watcher when a folder is
/// created, moved in or restored from the Trash).
pub(crate) fn index_tree(conn: &mut rusqlite::Connection, dir: &Path, filter: &PathFilter, gen: i64) -> Result<usize> {
    let mut files = Vec::new();
    let df = filter.clone();
    let walker = ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .follow_links(false)
        .filter_entry(move |e| {
            e.depth() == 0 || !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || df.allow_dir(e.path())
        })
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(kind) = filter.classify_file(entry.path()) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !filter.keep_size(entry.path(), &meta) {
            continue;
        }
        if let Some(f) = NewFile::from_path(entry.path(), kind, &meta) {
            files.push(f);
        }
    }
    let n = files.len();
    for chunk in files.chunks(BATCH) {
        write_rows(conn, chunk, gen, true)?;
    }
    Ok(n)
}

/// Remember (on this connection only) which rows a live update just wrote,
/// so the watcher reads their words even when their mtime is old: a document
/// moved or renamed gets a new row, and its text used to stay unread (out of
/// content search) until the next full pass, since the watcher only reads
/// files saved in the last hour.
pub(crate) const TOUCHED: &str = "CREATE TEMP TABLE IF NOT EXISTS lumen_touched (id INTEGER PRIMARY KEY)";


/// One name in a folder, compared the way macOS compares names: by
/// precomposed form.
fn same_name(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    a == b || matches!((a.to_str(), b.to_str()), (Some(a), Some(b)) if crate::nfc(a) == crate::nfc(b))
}

/// A name that has a single Unicode spelling (ASCII, Chinese, most others):
/// no folder listing needed to know how the disk spells it.
fn one_form(name: &std::ffi::OsStr) -> bool {
    use unicode_normalization::{is_nfc_quick, is_nfd_quick, IsNormalized};
    match name.to_str() {
        Some(s) => {
            s.is_ascii()
                || (is_nfc_quick(s.chars()) == IsNormalized::Yes && is_nfd_quick(s.chars()) == IsNormalized::Yes)
        }
        None => true,
    }
}

/// A path from a file-system event, spelled as the walk spells it.
struct Spelled {
    /// The configured root, then each name as the folder lists it, as far as
    /// those names exist; then the rest as the event spelled it.
    path: PathBuf,
    /// How many of `path`'s trailing names were not found on disk.
    missing: usize,
    /// Spellings the event used that the folder does not list: rows stored
    /// under them are not files on disk (left by an older Lumen, or a twin
    /// that was removed on a disk that tells the forms apart).
    stale: Vec<PathBuf>,
}

/// An event may spell an accented (or Hangul) name in the other Unicode
/// form than the folder lists it, and the disk opens either. The walk
/// stores the folder's spelling, so a live update in the event's spelling
/// added a second row for the file, and a removal left the first one.
/// Only names that can be spelled two ways are looked up (one folder
/// listing each, cached for the batch); ASCII and Chinese names are not.
fn spell_as_listed(
    p: &Path,
    roots: &[PathBuf],
    listings: &mut std::collections::HashMap<PathBuf, Vec<std::ffi::OsString>>,
) -> Spelled {
    let unchanged = || Spelled { path: p.to_path_buf(), missing: 0, stale: Vec::new() };
    if p.to_str().is_none_or(|s| s.is_ascii()) {
        return unchanged();
    }
    let comps: Vec<&std::ffi::OsStr> = p.iter().collect();
    // The innermost root it lies under, in the root's configured spelling.
    let root = roots
        .iter()
        .filter(|r| {
            let rc: Vec<&std::ffi::OsStr> = r.iter().collect();
            rc.len() <= comps.len() && rc.iter().zip(&comps).all(|(a, b)| same_name(a, b))
        })
        .max_by_key(|r| r.as_os_str().len());
    let Some(root) = root else { return unchanged() };
    let mut out = root.clone();
    let mut stale = Vec::new();
    let mut missing = 0;
    for name in &comps[root.iter().count()..] {
        if missing > 0 {
            out.push(name);
            missing += 1;
            continue;
        }
        if one_form(name) {
            out.push(name);
            continue;
        }
        let listed = listings.entry(out.clone()).or_insert_with(|| {
            std::fs::read_dir(&out).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default()
        });
        if listed.iter().any(|e| e == name) {
            out.push(name);
        } else if let Some(real) = listed.iter().find(|e| same_name(e, name)) {
            stale.push(out.join(name));
            out.push(real);
        } else {
            out.push(name);
            missing = 1;
        }
    }
    Spelled { path: out, missing, stale }
}

/// Every spelling of a path whose last `missing` names are gone: the walk
/// may have stored them either way.
fn gone_spellings(p: &Path, missing: usize) -> Vec<PathBuf> {
    use unicode_normalization::UnicodeNormalization;
    let mut out = vec![p.to_path_buf()];
    let comps: Vec<&std::ffi::OsStr> = p.iter().collect();
    if missing == 0 || missing > comps.len() {
        return out;
    }
    let (head, tail) = comps.split_at(comps.len() - missing);
    let Some(tail) = tail.iter().map(|c| c.to_str()).collect::<Option<Vec<&str>>>() else { return out };
    let base: PathBuf = head.iter().collect();
    for form in [tail.join("/").nfc().collect::<String>(), tail.join("/").nfd().collect::<String>()] {
        let q = base.join(form);
        if !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

/// `p` under the root it belongs to, when it was reported under the real
/// location of a root that is a link.
fn as_under_root(p: &Path, roots: &[PathBuf], targets: &[(PathBuf, &PathBuf)]) -> PathBuf {
    if roots.iter().any(|r| p.starts_with(r)) {
        return p.to_path_buf();
    }
    match targets.iter().filter(|(c, _)| p.starts_with(c)).max_by_key(|(c, _)| c.as_os_str().len()) {
        Some((c, r)) => r.join(p.strip_prefix(c).unwrap_or(p)),
        None => p.to_path_buf(),
    }
}

/// Apply a set of changed paths reported by the file system.
pub(crate) fn apply_paths(
    conn: &mut rusqlite::Connection,
    paths: &[PathBuf],
    filter: &PathFilter,
    roots: &[PathBuf],
) -> Result<(usize, usize)> {
    let gen = get_meta_i64(conn, "generation")?.unwrap_or(0).max(1);
    let (mut upserted, mut removed) = (0, 0);
    let mut listings = std::collections::HashMap::new();
    // A root that is a link (to a drive, or another folder): FSEvents reports
    // its files under the link's target, which lies under no root, so every
    // live change in it was dropped until the next scan.
    let targets: Vec<(PathBuf, &PathBuf)> = roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok().filter(|c| c != r).map(|c| (c, r)))
        .collect();
    for p in paths {
        let p = &as_under_root(p, roots, &targets);
        let spelled = spell_as_listed(p, roots, &mut listings);
        for s in &spelled.stale {
            removed += remove_tree(conn, &s.to_string_lossy())?;
        }
        let p = &spelled.path;
        if !filter.allow_path_chain(p, roots) {
            continue;
        }
        // The root itself is unreachable: a drive was ejected (or a link to
        // one). Removing its files here wiped an unplugged drive from the
        // index, which a scan keeps as offline. A root that was deleted
        // outright still has its files removed.
        if roots.iter().filter(|r| p.starts_with(r)).max_by_key(|r| r.as_os_str().len()).is_some_and(|r| root_offline(r)) {
            continue;
        }
        match std::fs::symlink_metadata(p) {
            Ok(meta) if meta.is_dir() => {
                if filter.allow_dir(p) {
                    upserted += index_tree(conn, p, filter, gen)?;
                }
            }
            Ok(meta) if meta.is_file() => {
                if let Some(kind) = filter.classify_file(p) {
                    if filter.keep_size(p, &meta) {
                        if let Some(f) = NewFile::from_path(p, kind, &meta) {
                            write_rows(conn, std::slice::from_ref(&f), gen, true)?;
                            upserted += 1;
                        }
                    } else {
                        // Emptied (or shrunk under the size floor): a full scan
                        // would drop it, so the live update does too, rather
                        // than keep showing its old size until then.
                        removed += remove_tree(conn, &p.to_string_lossy())?;
                    }
                }
            }
            Ok(_) => {}
            // Unreadable is not gone: a folder whose access was revoked (macOS
            // privacy settings, chmod) keeps its files listed. Only a path that
            // no longer exists is removed.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(_) => {
                for q in gone_spellings(p, spelled.missing) {
                    removed += remove_tree(conn, &q.to_string_lossy())?;
                }
            }
        }
    }
    Ok((upserted, removed))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn unreadable_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let locked = home.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let file = locked.join("plan.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![home.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(index.file_count().unwrap(), 1);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::symlink_metadata(&file).is_err();
        let mut conn = index.writer().unwrap();
        let r = apply_paths(&mut conn, std::slice::from_ref(&file), &cfg.filter(), &cfg.effective_roots());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !denied {
            return; // running as root: permissions are not enforced
        }
        assert_eq!(r.unwrap(), (0, 0));
        assert_eq!(index.file_count().unwrap(), 1);
        // A path that is really gone is still removed.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(apply_paths(&mut conn, &[file], &cfg.filter(), &cfg.effective_roots()).unwrap(), (0, 1));
    }

    #[test]
    fn root_that_went_away_is_left_to_the_rescan() {
        // An ejected drive's root disappears and the watcher hears about it:
        // its files used to be deleted from the index on the spot, though a
        // scan keeps an unplugged drive's files as offline.
        let dir = tempfile::tempdir().unwrap();
        // (/Volumes cannot be made here; a root that links to the drive is
        // the same case: the entry is there, what it points at is not.)
        let target = dir.path().join("mnt/Passport");
        std::fs::create_dir_all(target.join("Movies")).unwrap();
        std::fs::write(target.join("Movies/trip.mp4"), b"mp4").unwrap();
        let drive = dir.path().join("Passport");
        std::os::unix::fs::symlink(&target, &drive).unwrap();
        let file = drive.join("Movies/trip.mp4");
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("a.pdf"), b"pdf").unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![drive.clone(), plain.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(index.file_count().unwrap(), 2);
        let (filter, roots) = (cfg.filter(), cfg.effective_roots());
        std::fs::rename(&target, dir.path().join("elsewhere")).unwrap(); // "unmounted"
        let mut conn = index.writer().unwrap();
        assert_eq!(apply_paths(&mut conn, &[drive.clone(), file.clone()], &filter, &roots).unwrap(), (0, 0));
        assert_eq!(index.file_count().unwrap(), 2);
        // A root folder deleted outright still loses its files.
        std::fs::remove_dir_all(&plain).unwrap();
        assert_eq!(apply_paths(&mut conn, std::slice::from_ref(&plain), &filter, &roots).unwrap(), (0, 1));
    }

    #[test]
    fn emptied_file_leaves_the_index_live() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let file = home.join("notes.txt");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&file, b"some notes").unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![home.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        std::fs::write(&file, b"").unwrap();
        let mut conn = index.writer().unwrap();
        assert_eq!(apply_paths(&mut conn, &[file], &cfg.filter(), &cfg.effective_roots()).unwrap(), (0, 1));
        assert_eq!(index.file_count().unwrap(), 0);
    }

    #[test]
    fn event_in_the_other_unicode_form_is_the_same_file() {
        // The disk keeps "Résumé/Café.txt" decomposed (NFD); a file-system
        // event names it precomposed (NFC), or the other way round. That used
        // to add a second row for the same file, and a removal reported in
        // the other form left the old row behind.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let folder_nfd = home.join("Re\u{301}sume\u{301}");
        let file_nfd = folder_nfd.join("Cafe\u{301}.txt");
        let file_nfc = home.join("R\u{e9}sum\u{e9}").join("Caf\u{e9}.txt");
        std::fs::create_dir_all(&folder_nfd).unwrap();
        std::fs::write(&file_nfd, b"some notes").unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![home.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        let (filter, roots) = (cfg.filter(), cfg.effective_roots());
        let mut conn = index.writer().unwrap();

        std::fs::write(&file_nfd, b"some longer notes").unwrap();
        assert_eq!(apply_paths(&mut conn, std::slice::from_ref(&file_nfc), &filter, &roots).unwrap(), (1, 0));
        assert_eq!(index.file_count().unwrap(), 1, "a second row for the same file");
        let size: i64 = conn.query_row("SELECT size FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(size, 17);
        // Its folder reported in the other form: same rows again.
        apply_paths(&mut conn, &[home.join("R\u{e9}sum\u{e9}")], &filter, &roots).unwrap();
        assert_eq!(index.file_count().unwrap(), 1);

        // A file of the other form that really is a different file (a disk
        // that tells the two apart) is not removed for the gone one.
        std::fs::write(folder_nfd.join("Caf\u{e9}.txt"), b"twin file").unwrap();
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(index.file_count().unwrap(), 2);
        std::fs::remove_file(folder_nfd.join("Caf\u{e9}.txt")).unwrap();
        apply_paths(&mut conn, std::slice::from_ref(&file_nfc), &filter, &roots).unwrap();
        assert_eq!(index.file_count().unwrap(), 1);
        let left: String = conn.query_row("SELECT path FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(left, file_nfd.to_string_lossy());

        // Removed, reported in the other form.
        std::fs::remove_file(&file_nfd).unwrap();
        assert_eq!(apply_paths(&mut conn, &[file_nfc], &filter, &roots).unwrap(), (0, 1));
        assert_eq!(index.file_count().unwrap(), 0, "the gone file stayed listed");
        // A rescan agrees.
        let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(r.added + r.removed, 0);
    }

    #[test]
    fn root_named_in_the_other_unicode_form() {
        // The root folder itself has an accented name and events spell it
        // the other way: they used to fall outside every root and be dropped.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("Docume\u{301}nts");
        std::fs::create_dir_all(&home).unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![home.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        std::fs::write(home.join("plan.pdf"), b"pdf").unwrap();
        let mut conn = index.writer().unwrap();
        let event = dir.path().join("Docum\u{e9}nts/plan.pdf");
        assert_eq!(apply_paths(&mut conn, &[event], &cfg.filter(), &cfg.effective_roots()).unwrap(), (1, 0));
        let p: String = conn.query_row("SELECT path FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(p, home.join("plan.pdf").to_string_lossy());
    }

    #[test]
    fn root_that_is_a_link_gets_live_updates() {
        // FSEvents names changes by the real location, not the link the root
        // was chosen through.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real/Notes");
        std::fs::create_dir_all(&target).unwrap();
        let link = dir.path().join("Notes");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let index = Index::open(dir.path().join("index.db")).unwrap();
        let cfg = Config { roots: vec![link.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        std::fs::write(target.join("plan.pdf"), b"pdf").unwrap();
        let event = std::fs::canonicalize(&target).unwrap().join("plan.pdf");
        let mut conn = index.writer().unwrap();
        assert_eq!(apply_paths(&mut conn, std::slice::from_ref(&event), &cfg.filter(), &cfg.effective_roots()).unwrap(), (1, 0));
        let p: String = conn.query_row("SELECT path FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(p, link.join("plan.pdf").to_string_lossy(), "stored as the scan stores it");
        std::fs::remove_file(target.join("plan.pdf")).unwrap();
        assert_eq!(apply_paths(&mut conn, &[event], &cfg.filter(), &cfg.effective_roots()).unwrap(), (0, 1));
    }
}

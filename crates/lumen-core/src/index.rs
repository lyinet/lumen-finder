//! The on-disk index: one SQLite file (WAL), a row per file, and an FTS5
//! trigram table over names, pinyin and folders so substring search in any
//! language is instant.

use crate::kind::Kind;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    name_py     TEXT NOT NULL DEFAULT '',
    parent      TEXT NOT NULL,
    ext         TEXT NOT NULL,
    kind        INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    mtime       INTEGER NOT NULL,
    ctime       INTEGER NOT NULL,
    first_gen   INTEGER NOT NULL,
    seen_gen    INTEGER NOT NULL,
    changed_gen INTEGER NOT NULL,
    hash        TEXT,
    cloud       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS files_kind_mtime ON files(kind, mtime DESC);
CREATE INDEX IF NOT EXISTS files_mtime ON files(mtime DESC);
CREATE INDEX IF NOT EXISTS files_size ON files(size DESC);
CREATE INDEX IF NOT EXISTS files_parent ON files(parent);
CREATE INDEX IF NOT EXISTS files_seen ON files(seen_gen);

CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
    name, name_py, parent,
    content='files', content_rowid='id',
    tokenize='trigram'
);

-- Name and folder never change for a given path, so rescans that only touch
-- size/mtime/seen_gen leave the full-text index alone.
CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
    INSERT INTO files_fts(rowid, name, name_py, parent)
    VALUES (new.id, new.name, new.name_py, new.parent);
END;
CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
    INSERT INTO files_fts(files_fts, rowid, name, name_py, parent)
    VALUES ('delete', old.id, old.name, old.name_py, old.parent);
END;
CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE OF name, name_py, parent ON files BEGIN
    INSERT INTO files_fts(files_fts, rowid, name, name_py, parent)
    VALUES ('delete', old.id, old.name, old.name_py, old.parent);
    INSERT INTO files_fts(rowid, name, name_py, parent)
    VALUES (new.id, new.name, new.name_py, new.parent);
END;

CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- v2: the words inside documents. One row per readable file, keyed by the
-- file's id; size/mtime say which version of the file the text came from.
CREATE TABLE IF NOT EXISTS content (
    id    INTEGER PRIMARY KEY,
    mtime INTEGER NOT NULL,
    size  INTEGER NOT NULL,
    body  TEXT NOT NULL DEFAULT '',
    ocr   INTEGER NOT NULL DEFAULT 0
);
-- v3: the index holds the text split by cjk::index_text (CJK runs as
-- overlapping two-character pieces), written from Rust; the stored body above
-- is only for snippets.
CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
    toks, content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS content_ad3 AFTER DELETE ON content BEGIN
    DELETE FROM content_fts WHERE rowid = old.id;
END;
CREATE TRIGGER IF NOT EXISTS files_ad_content AFTER DELETE ON files BEGIN
    DELETE FROM content WHERE id = old.id;
END;

-- v0.6: one sentence vector per file for semantic search (see semantic.rs).
-- size/mtime say which version of the file it describes; an empty vec means
-- the model had nothing for it.
CREATE TABLE IF NOT EXISTS vectors (
    id    INTEGER PRIMARY KEY,
    mtime INTEGER NOT NULL,
    size  INTEGER NOT NULL,
    model INTEGER NOT NULL,
    vec   BLOB NOT NULL
);
CREATE TRIGGER IF NOT EXISTS files_ad_vectors AFTER DELETE ON files BEGIN
    DELETE FROM vectors WHERE id = old.id;
END;

-- v0.8: files the user opened from Lumen, so search puts them first.
CREATE TABLE IF NOT EXISTS opens (
    id    INTEGER PRIMARY KEY,
    count INTEGER NOT NULL,
    last  INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS files_ad_opens AFTER DELETE ON files BEGIN
    DELETE FROM opens WHERE id = old.id;
END;
"#;

pub(crate) const UPSERT: &str = r#"
INSERT INTO files (path, name, name_py, parent, ext, kind, size, mtime, ctime,
                   first_gen, seen_gen, changed_gen, cloud)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10, ?11)
ON CONFLICT(path) DO UPDATE SET
    seen_gen    = excluded.seen_gen,
    changed_gen = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime
                       THEN excluded.seen_gen ELSE files.changed_gen END,
    hash        = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime
                       THEN NULL ELSE files.hash END,
    size        = excluded.size,
    mtime       = excluded.mtime,
    kind        = excluded.kind,
    cloud       = excluded.cloud
"#;

/// A file as the index knows it.
#[derive(Debug, Clone, Serialize)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub parent: String,
    pub ext: String,
    pub kind: Kind,
    pub size: i64,
    pub mtime: i64,
    /// On an external drive that is not plugged in right now.
    pub offline: bool,
    /// In iCloud Drive or a cloud sync folder and not downloaded to this Mac.
    pub cloud: bool,
    /// The cloud service that syncs it, from where it lies (v0.9): iCloud
    /// Drive, Dropbox, OneDrive, Google Drive …, downloaded or not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::config::Provider>,
    /// Where the query matched inside the file, when it matched the words
    /// rather than the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// The snippet's text was read by OCR (a scanned PDF, an image).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ocr: bool,
}

impl FileRecord {
    pub(crate) const COLS: &'static str = "f.id, f.path, f.name, f.parent, f.ext, f.kind, f.size, f.mtime, f.cloud";

    pub(crate) fn from_row(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        let path: String = r.get(1)?;
        Ok(FileRecord {
            id: r.get(0)?,
            offline: crate::config::volume_offline(Path::new(&path)),
            provider: crate::config::provider_of(&path, home()),
            snippet: None,
            ocr: false,
            path,
            name: r.get(2)?,
            parent: r.get(3)?,
            ext: r.get(4)?,
            kind: Kind::from_i64(r.get(5)?).unwrap_or(Kind::Document),
            size: r.get(6)?,
            mtime: r.get(7)?,
            cloud: r.get::<_, i64>(8)? != 0,
        })
    }
}

/// Everything the writer needs to store one file.
#[derive(Debug, Clone)]
pub struct NewFile {
    pub path: String,
    pub name: String,
    pub name_py: String,
    pub parent: String,
    pub ext: String,
    pub kind: Kind,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub cloud: bool,
}

impl NewFile {
    pub fn from_path(path: &Path, kind: Kind, meta: &std::fs::Metadata) -> Option<NewFile> {
        let file_name = path.file_name()?.to_string_lossy().into_owned();
        let placeholder = crate::config::icloud_placeholder(&file_name).map(str::to_string);
        let cloud = placeholder.is_some() || is_dataless(meta);
        // Shown and searched precomposed; `path` keeps the disk's spelling.
        let name = crate::nfc(&placeholder.unwrap_or(file_name)).into_owned();
        let parent = path.parent().map(|p| crate::nfc(&p.to_string_lossy()).into_owned()).unwrap_or_default();
        let ext = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        // A placeholder's own size is a few hundred bytes of plist, not the file's.
        let size = if cloud && !is_dataless(meta) { 0 } else { meta.len() as i64 };
        let mtime = meta.modified().ok().map(unix).unwrap_or(0);
        let ctime = meta.created().ok().map(unix).unwrap_or(mtime);
        Some(NewFile {
            path: path.to_string_lossy().into_owned(),
            name_py: crate::name_key(&name),
            name,
            parent,
            ext,
            kind,
            size,
            mtime,
            ctime,
            cloud,
        })
    }

    pub(crate) fn upsert(&self, stmt: &mut rusqlite::CachedStatement, gen: i64) -> rusqlite::Result<usize> {
        stmt.execute(params![
            self.path,
            self.name,
            self.name_py,
            self.parent,
            self.ext,
            self.kind.as_i64(),
            self.size,
            self.mtime,
            self.ctime,
            gen,
            self.cloud as i64
        ])
    }
}

/// The home folder, looked up once (labels every result's provider).
pub(crate) fn home() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(dirs::home_dir).as_deref()
}

/// The file's bytes are on this Mac right now: it exists and is neither an
/// iCloud placeholder nor a dataless cloud file. Every path that opens a
/// file's contents in the background (text, OCR, duplicate hashes) asks this
/// first, at the moment of reading: the index's `cloud` flag can be older
/// than an eviction ("Remove Download", Optimize Storage, a File Provider
/// freeing space), and opening a dataless file makes macOS download it.
/// Looking at the metadata never does.
pub fn is_local(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && !is_cloud(path, &meta),
        Err(_) => false,
    }
}

/// Not downloaded from iCloud: a ".name.icloud" placeholder (macOS 13 and
/// earlier) or a dataless file (macOS 14+, SF_DATALESS; also every File
/// Provider sync folder in ~/Library/CloudStorage: Dropbox, OneDrive,
/// Google Drive …). Reading one would
/// make macOS download it, so Lumen never opens these.
pub fn is_cloud(path: &Path, meta: &std::fs::Metadata) -> bool {
    is_dataless(meta)
        || path.file_name().and_then(|n| n.to_str()).and_then(crate::config::icloud_placeholder).is_some()
}

#[cfg(target_os = "macos")]
fn is_dataless(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_meta: &std::fs::Metadata) -> bool {
    false
}

pub(crate) fn unix(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// A count or offset for SQL: clamped, since `usize as i64` wraps to -1.
pub(crate) fn sql_int(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn now() -> i64 {
    unix(std::time::SystemTime::now())
}

pub struct Index {
    path: PathBuf,
    conn: Mutex<Connection>,
    /// Whole-index summaries (insights, timeline, collections) by name.
    memo: Mutex<std::collections::HashMap<&'static str, Memo>>,
}

struct Memo {
    version: i64,
    value: Box<dyn std::any::Any + Send>,
}


impl Index {
    /// Open the index, or, when the file is damaged (a crash mid-write, a
    /// bad disk sector), set it aside and start a fresh one. The index is a
    /// cache of the disk, so a rescan rebuilds everything. `true` means it
    /// was rebuilt.
    pub fn open_or_reset(path: impl AsRef<Path>) -> Result<(Index, bool)> {
        // v0.8.0 ran `PRAGMA quick_check` here, which reads the whole file
        // before the window can appear: seconds on a big index, and on a busy
        // or locked file (a previous Lumen still quitting) it failed and set a
        // healthy index aside. Launch now only opens the file and reads its
        // schema; it starts over only when SQLite itself says the file is not
        // a database or is corrupt. `check_integrity` runs the full check
        // later, in the background.
        let path = path.as_ref();
        match Index::open(path).and_then(|idx| {
            idx.conn().query_row("SELECT count(*) FROM meta", [], |r| r.get::<_, i64>(0))?;
            Ok(idx)
        }) {
            Ok(idx) => Ok((idx, false)),
            Err(e) if is_corrupt(&e) => {
                log::warn!("index at {} is damaged ({e:#}); starting a new one", path.display());
                Index::set_aside(path);
                Ok((Index::open(path)?, true))
            }
            Err(e) => Err(e),
        }
    }

    fn set_aside(path: &Path) {
        let aside = path.with_extension(format!("damaged-{}.db", now()));
        let _ = std::fs::rename(path, &aside);
        for ext in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
    }

    /// The full integrity check, for a background thread after launch.
    /// `Ok(false)` means the index is damaged and should be rebuilt; a busy
    /// or locked database is an error, never "damaged".
    pub fn check_integrity(&self) -> Result<bool> {
        let r: rusqlite::Result<String> = self.conn().query_row("PRAGMA quick_check", [], |r| r.get(0));
        match r {
            Ok(ok) => Ok(ok == "ok"),
            Err(e)
                if matches!(
                    e.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Write files straight into the index as one scan generation. For load
    /// tests and tools; the app always goes through `scan`.
    #[doc(hidden)]
    pub fn insert_files(&self, files: &[NewFile]) -> Result<()> {
        let mut conn = self.writer()?;
        let gen = get_meta_i64(&conn, "generation")?.unwrap_or(0) + 1;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(UPSERT)?;
            for f in files {
                f.upsert(&mut stmt, gen)?;
            }
        }
        set_meta(&tx, "generation", gen)?;
        tx.commit()?;
        Ok(())
    }

    /// Forget everything; the next scan is a first scan again.
    pub fn reset(&self) -> Result<()> {
        let conn = self.conn();
        // Row ids restart from 1 after this, so the vector revision must keep
        // counting up: restarting it too let semantic search reuse a cached
        // copy of the old vectors under the same (path, revision) key and
        // answer with whichever new file got an old id.
        let vec_rev = get_meta_i64(&conn, "vec_rev")?.unwrap_or(0);
        conn.execute_batch(
            "BEGIN;
             DELETE FROM opens; DELETE FROM vectors; DELETE FROM content;
             DELETE FROM files; DELETE FROM meta;
             COMMIT;",
        )?;
        set_meta(&conn, "vec_rev", vec_rev + 1)?;
        conn.execute_batch("VACUUM;")?;
        drop(conn);
        self.forget_memos();
        Ok(())
    }

    /// Remember that the user opened this file from Lumen.
    pub fn record_open(&self, path: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO opens(id, count, last) SELECT id, 1, ?2 FROM files WHERE path = ?1
             ON CONFLICT(id) DO UPDATE SET count = count + 1, last = excluded.last",
            params![path, now()],
        )?;
        Ok(())
    }

    /// Files opened from Lumen, most recent first.
    pub fn recently_opened(&self, limit: usize) -> Result<Vec<FileRecord>> {
        let sql = format!(
            "SELECT {} FROM opens o JOIN files f ON f.id = o.id ORDER BY o.last DESC LIMIT ?1",
            FileRecord::COLS
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], FileRecord::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Searches worth trying right after the first scan, drawn from the
    /// user's own files: pinyin initials of a recent Chinese-named document
    /// ("nzzj" → 年终总结.key), and a recent English name fragment.
    pub fn suggestions(&self, limit: usize) -> Result<Vec<Suggestion>> {
        let conn = self.conn();
        let mut out: Vec<Suggestion> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT name, name_py FROM files WHERE name_py <> '' AND cloud = 0
               AND kind IN (0, 1, 2, 3, 6)
             ORDER BY mtime DESC LIMIT 400",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for (name, py) in rows.flatten() {
            let stem = name.rsplit_once('.').map(|x| x.0).unwrap_or(&name);
            let n = stem.chars().count();
            // (After the folded name of an accented one, see `crate::name_key`.)
            let py = py.rsplit_once('\t').map(|(_, p)| p).unwrap_or(&py);
            let initials = py.rsplit(' ').next().unwrap_or("");
            if !(3..=8).contains(&n) || !(3..=6).contains(&initials.len()) || !initials.chars().all(|c| c.is_ascii_lowercase()) {
                continue;
            }
            if out.iter().any(|s| s.query == initials) {
                continue;
            }
            out.push(Suggestion { query: initials.to_string(), name });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Index> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = open_conn(&path)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // v1 → v2 only added tables, which SCHEMA creates below.
        if version == 2 {
            // v2 kept a trigram index of document text. Drop it and forget the
            // text so the next pass re-reads documents into the v3 index.
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS content_ai; DROP TRIGGER IF EXISTS content_ad;
                 DROP TRIGGER IF EXISTS content_au; DROP TABLE IF EXISTS content_fts;
                 DELETE FROM content;",
            )?;
        }
        if version != 0 && version != SCHEMA_VERSION && version != 1 && version != 2 {
            // Future migrations go here. The index is a cache of the disk, so
            // the safe fallback is always "drop and rescan".
            // Everything keyed by a file id goes too: ids restart once `files`
            // is recreated, and leftover text, vectors or opens would be
            // attached to whichever new file got the old id.
            conn.execute_batch(
                "DROP TABLE IF EXISTS files_fts; DROP TABLE IF EXISTS vectors; DROP TABLE IF EXISTS files;
                 DROP TABLE IF EXISTS meta; DROP TABLE IF EXISTS content_fts; DROP TABLE IF EXISTS content;
                 DROP TABLE IF EXISTS opens;",
            )?;
        }
        conn.execute_batch(SCHEMA).context("creating schema")?;
        // v0.7: files.cloud (iCloud files not on this Mac).
        let has_cloud: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'cloud'")?
            .exists([])?;
        if !has_cloud {
            conn.execute_batch("ALTER TABLE files ADD COLUMN cloud INTEGER NOT NULL DEFAULT 0;")?;
        }
        // v0.8.3: content.ocr (text read by OCR, labelled "in image").
        let has_ocr: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('content') WHERE name = 'ocr'")?
            .exists([])?;
        if !has_ocr {
            conn.execute_batch("ALTER TABLE content ADD COLUMN ocr INTEGER NOT NULL DEFAULT 0;")?;
        }
        // Only write when something changed: a write waits for any other
        // connection's transaction (a scan still finishing in a Lumen that is
        // quitting), and launch must not.
        if version != SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Index { path, conn: Mutex::new(conn), memo: Default::default() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The shared read connection. Writers (scanner, watcher) open their own,
    /// so searching never waits behind a scan.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `compute` once per change of the index, shared by every caller.
    /// SQLite's data_version moves whenever another connection (the scanner,
    /// the watcher) commits, which is exactly when a summary goes stale.
    pub(crate) fn memo<T: Clone + Send + 'static>(
        &self,
        key: &'static str,
        compute: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let conn = self.conn();
        let version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        {
            let memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = memo.get(key) {
                if m.version == version {
                    if let Some(v) = m.value.downcast_ref::<T>() {
                        return Ok(v.clone());
                    }
                }
            }
        }
        let value = compute(&conn)?;
        self.memo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Memo { version, value: Box::new(value.clone()) });
        Ok(value)
    }

    pub(crate) fn forget_memos(&self) {
        self.memo.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub(crate) fn writer(&self) -> Result<Connection> {
        open_conn(&self.path)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn file_count(&self) -> Result<i64> {
        Ok(self.conn().query_row("SELECT count(*) FROM files", [], |r| r.get(0))?)
    }

    pub fn status(&self) -> Result<IndexStatus> {
        let (files, bytes): (i64, i64) = self.conn().query_row(
            "SELECT count(*), coalesce(sum(size), 0) FROM files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let num = |k: &str| -> Result<Option<i64>> { Ok(self.meta(k)?.and_then(|v| v.parse().ok())) };
        Ok(IndexStatus {
            files,
            bytes,
            last_scan_at: num("last_scan_at")?,
            last_scan_ms: num("last_scan_ms")?,
            generation: num("generation")?.unwrap_or(0),
            content_files: self.conn().query_row("SELECT count(*) FROM content WHERE body <> ''", [], |r| r.get(0))?,
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<FileRecord>> {
        let sql = format!("SELECT {} FROM files f WHERE f.id = ?1", FileRecord::COLS);
        Ok(self.conn().query_row(&sql, [id], FileRecord::from_row).optional()?)
    }

    /// Forget a file Lumen found missing when the user tried to open it.
    pub fn remove_path(&self, path: &str) -> Result<usize> {
        let n = remove_tree(&self.conn(), path)?;
        // PRAGMA data_version only moves for other connections' commits, so a
        // write on this one must drop the summaries itself.
        if n > 0 {
            self.forget_memos();
        }
        Ok(n)
    }

    /// The stored text of a file, if Lumen has read it.
    pub fn content(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT body FROM content WHERE id = ?1 AND body <> ''", [id], |r| r.get(0))
            .optional()?)
    }
}

/// A search to try, and the file it finds.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Suggestion {
    pub query: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub files: i64,
    pub bytes: i64,
    pub last_scan_at: Option<i64>,
    pub last_scan_ms: Option<i64>,
    pub generation: i64,
    /// Documents whose words are searchable.
    pub content_files: i64,
}

/// SQLite says the file is not a database, or is corrupt. Busy, locked,
/// permission and disk-full errors are not damage.
fn is_corrupt(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<rusqlite::Error>().and_then(|e| e.sqlite_error_code()),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        )
    })
}

pub(crate) fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32000;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(conn)
}

pub(crate) fn set_meta(conn: &Connection, key: &str, value: impl ToString) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

pub(crate) fn get_meta_i64(conn: &Connection, key: &str) -> rusqlite::Result<Option<i64>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()))
}

/// Delete a path and, when it was a folder, everything under it.
pub(crate) fn remove_tree(conn: &Connection, path: &str) -> Result<usize> {
    let trimmed = path.trim_end_matches('/');
    let prefix = format!("{}/", trimmed);
    Ok(conn.execute(
        "DELETE FROM files WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
        params![trimmed, prefix, format!("{}0", trimmed)], // '0' is the byte after '/'
    )?)
}

//! The on-disk index: one SQLite file (WAL), a row per file, and an FTS5
//! trigram table over names, pinyin and folders so substring search in any
//! language is instant.

use crate::kind::Kind;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const SCHEMA_VERSION: i64 = 1;

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
    hash        TEXT
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
"#;

pub(crate) const UPSERT: &str = r#"
INSERT INTO files (path, name, name_py, parent, ext, kind, size, mtime, ctime,
                   first_gen, seen_gen, changed_gen)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)
ON CONFLICT(path) DO UPDATE SET
    seen_gen    = excluded.seen_gen,
    changed_gen = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime
                       THEN excluded.seen_gen ELSE files.changed_gen END,
    hash        = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime
                       THEN NULL ELSE files.hash END,
    size        = excluded.size,
    mtime       = excluded.mtime,
    kind        = excluded.kind
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
}

impl FileRecord {
    pub(crate) const COLS: &'static str = "f.id, f.path, f.name, f.parent, f.ext, f.kind, f.size, f.mtime";

    pub(crate) fn from_row(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get(0)?,
            path: r.get(1)?,
            name: r.get(2)?,
            parent: r.get(3)?,
            ext: r.get(4)?,
            kind: Kind::from_i64(r.get(5)?).unwrap_or(Kind::Document),
            size: r.get(6)?,
            mtime: r.get(7)?,
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
}

impl NewFile {
    pub fn from_path(path: &Path, kind: Kind, meta: &std::fs::Metadata) -> Option<NewFile> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let parent = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mtime = meta.modified().ok().map(unix).unwrap_or(0);
        let ctime = meta.created().ok().map(unix).unwrap_or(mtime);
        Some(NewFile {
            path: path.to_string_lossy().into_owned(),
            name_py: crate::pinyin_key(&name),
            name,
            parent,
            ext,
            kind,
            size: meta.len() as i64,
            mtime,
            ctime,
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
            gen
        ])
    }
}

pub(crate) fn unix(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub(crate) fn now() -> i64 {
    unix(std::time::SystemTime::now())
}

pub struct Index {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl Index {
    pub fn open(path: impl AsRef<Path>) -> Result<Index> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = open_conn(&path)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 0 && version != SCHEMA_VERSION {
            // Future migrations go here. The index is a cache of the disk, so
            // the safe fallback is always "drop and rescan".
            conn.execute_batch("DROP TABLE IF EXISTS files_fts; DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS meta;")?;
        }
        conn.execute_batch(SCHEMA).context("creating schema")?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Index { path, conn: Mutex::new(conn) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The shared read connection. Writers (scanner, watcher) open their own,
    /// so searching never waits behind a scan.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
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
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<FileRecord>> {
        let sql = format!("SELECT {} FROM files f WHERE f.id = ?1", FileRecord::COLS);
        Ok(self.conn().query_row(&sql, [id], FileRecord::from_row).optional()?)
    }

    /// Forget a file Lumen found missing when the user tried to open it.
    pub fn remove_path(&self, path: &str) -> Result<usize> {
        remove_tree(&self.conn(), path)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub files: i64,
    pub bytes: i64,
    pub last_scan_at: Option<i64>,
    pub last_scan_ms: Option<i64>,
    pub generation: i64,
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

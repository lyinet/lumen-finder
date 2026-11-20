//! The "where did all my stuff go" views: totals by kind, how scattered each
//! kind is, recent activity, space hogs, forgotten files and duplicates.

use crate::index::{now, FileRecord, Index};
use crate::kind::Kind;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Serialize)]
pub struct KindStat {
    pub kind: Kind,
    pub files: i64,
    pub bytes: i64,
    /// Number of distinct folders holding this kind: "your PDFs live in 214 folders".
    pub folders: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderStat {
    pub path: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub kinds: Vec<KindStat>,
    pub top_folders: Vec<FolderStat>,
    pub added_7d: i64,
    pub changed_7d: i64,
    pub forgotten_files: i64,
    pub forgotten_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    pub size: i64,
    pub files: Vec<FileRecord>,
    /// Bytes freed by keeping one copy.
    pub wasted: i64,
}

/// Untouched for this long counts as forgotten.
pub const FORGOTTEN_AFTER_SECS: i64 = 2 * 365 * 86_400;
const FORGOTTEN_MIN_SIZE: i64 = 50 * 1024 * 1024;

impl Index {
    pub fn overview(&self) -> Result<Overview> {
        self.memo("overview", |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT kind, count(*), coalesce(sum(size),0), count(DISTINCT parent)
                 FROM files GROUP BY kind ORDER BY sum(size) DESC",
            )?;
            let kinds = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .flatten()
                .filter_map(|(k, files, bytes, folders)| {
                    Kind::from_i64(k).map(|kind| KindStat { kind, files, bytes, folders })
                })
                .collect();

            let mut stmt = conn.prepare_cached(
                "SELECT parent, count(*), sum(size) FROM files GROUP BY parent ORDER BY count(*) DESC LIMIT 8",
            )?;
            let top_folders = stmt
                .query_map([], |r| Ok(FolderStat { path: r.get(0)?, files: r.get(1)?, bytes: r.get(2)? }))?
                .flatten()
                .collect();

            let week = now() - 7 * 86_400;
            let (added_7d, changed_7d): (i64, i64) = conn.query_row(
                "SELECT coalesce(sum(ctime >= ?1),0), coalesce(sum(mtime >= ?1),0) FROM files",
                [week],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (forgotten_files, forgotten_bytes): (i64, i64) = conn.query_row(
                "SELECT count(*), coalesce(sum(size),0) FROM files WHERE mtime < ?1 AND size >= ?2",
                params![now() - FORGOTTEN_AFTER_SECS, FORGOTTEN_MIN_SIZE],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(Overview { kinds, top_folders, added_7d, changed_7d, forgotten_files, forgotten_bytes })
        })
    }

    fn records(&self, sql_tail: &str, args: impl rusqlite::Params) -> Result<Vec<FileRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!("SELECT {} FROM files f {sql_tail}", FileRecord::COLS))?;
        let v = stmt.query_map(args, FileRecord::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<FileRecord>> {
        self.records("ORDER BY f.mtime DESC LIMIT ?1", [limit as i64])
    }

    pub fn largest(&self, limit: usize) -> Result<Vec<FileRecord>> {
        self.records("ORDER BY f.size DESC LIMIT ?1", [limit as i64])
    }

    /// Big files nobody has touched in two years.
    pub fn forgotten(&self, limit: usize) -> Result<Vec<FileRecord>> {
        self.records(
            "WHERE f.mtime < ?1 AND f.size >= ?2 ORDER BY f.size DESC LIMIT ?3",
            params![now() - FORGOTTEN_AFTER_SECS, FORGOTTEN_MIN_SIZE, limit as i64],
        )
    }

    /// Same-content files. Candidates share a size; content is compared by a
    /// sampled BLAKE3 fingerprint (head, middle, tail + size), cached per row
    /// and invalidated automatically when the file changes.
    pub fn duplicates(&self, min_size: i64, max_groups: usize) -> Result<Vec<DuplicateGroup>> {
        self.duplicates_cancellable(min_size, max_groups, &AtomicBool::new(false))
    }

    /// `duplicates`, stopping as soon as `cancel` is set (v0.9): the first
    /// look at many huge files of one size hashes every byte of them, which
    /// could not be stopped before. Hashes already finished stay cached, so
    /// the next look resumes. When cancelled, the groups found so far are
    /// returned (complete groups only); the caller knows it asked to stop.
    pub fn duplicates_cancellable(&self, min_size: i64, max_groups: usize, cancel: &AtomicBool) -> Result<Vec<DuplicateGroup>> {
        let sizes: Vec<i64> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT size FROM files WHERE size >= ?1 AND cloud = 0 GROUP BY size HAVING count(*) > 1
                 ORDER BY size DESC LIMIT ?2",
            )?;
            // max_groups comes from callers: usize::MAX * 4 must not overflow.
            let cap = i64::try_from(max_groups.saturating_mul(4)).unwrap_or(i64::MAX);
            let v = stmt.query_map(params![min_size.max(1), cap], |r| r.get(0))?.flatten().collect();
            v
        };
        let mut groups = Vec::new();
        for size in sizes {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let cands: Vec<(i64, String, Option<String>)> = {
                let conn = self.conn();
                let mut stmt = conn.prepare_cached("SELECT id, path, hash FROM files WHERE size = ?1 AND cloud = 0")?;
                let v = stmt.query_map([size], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.flatten().collect();
                v
            };
            // The cached value is the sampled fingerprint, followed by
            // ":<full hash>" once the file has matched another one's samples.
            let mut by_sample: std::collections::HashMap<String, Vec<(i64, String, Option<String>)>> = Default::default();
            for (id, path, hash) in cands {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let (sample, full) = match hash.as_deref().map(|h| h.split_once(':').unwrap_or((h, ""))) {
                    Some((s, f)) => (s.to_string(), (!f.is_empty()).then(|| f.to_string())),
                    // Only in the cloud now (evicted since the scan): reading
                    // it would download it. Skipped, like an unreadable file.
                    None if !crate::index::is_local(std::path::Path::new(&path)) => continue,
                    None => match fingerprint(&path, size as u64) {
                        Ok(h) => {
                            self.conn().execute("UPDATE files SET hash = ?1 WHERE id = ?2", params![h, id])?;
                            (h, None)
                        }
                        Err(_) => continue, // unreadable or gone; skip
                    },
                };
                by_sample.entry(sample).or_default().push((id, path, full));
            }
            // Samples only cover head, middle and tail: files that agree on
            // them are compared in full before they count as copies (two disk
            // images or recordings the same size often agree there).
            let mut by_hash: std::collections::HashMap<String, Vec<i64>> = Default::default();
            for (sample, members) in by_sample.into_iter().filter(|(_, v)| v.len() > 1) {
                for (id, path, full) in members {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let h = if size as u64 <= SAMPLE * 3 {
                        sample.clone() // the sample was the whole file
                    } else if let Some(f) = full {
                        f
                    } else if !crate::index::is_local(std::path::Path::new(&path)) {
                        continue;
                    } else {
                        match full_hash(&path, cancel) {
                            Ok(f) => {
                                self.conn().execute(
                                    "UPDATE files SET hash = ?1 WHERE id = ?2",
                                    params![format!("{sample}:{f}"), id],
                                )?;
                                f
                            }
                            Err(_) => continue,
                        }
                    };
                    by_hash.entry(h).or_default().push(id);
                }
            }
            // Stopped part way through this size: its groups may be missing
            // members, so none of them is reported.
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            for ids in by_hash.into_values().filter(|v| v.len() > 1) {
                let files: Vec<FileRecord> = ids.iter().filter_map(|id| self.get(*id).ok().flatten()).collect();
                // A copy can leave the index while this runs (the watcher):
                // one file left is not a duplicate, and none would be -size wasted.
                if files.len() < 2 {
                    continue;
                }
                let wasted = size * (files.len() as i64 - 1);
                groups.push(DuplicateGroup { size, files, wasted });
            }
            if groups.len() >= max_groups {
                break;
            }
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g.wasted));
        groups.truncate(max_groups);
        Ok(groups)
    }
}

const SAMPLE: u64 = 256 * 1024;

/// Every byte of the file, a few MB at a time, giving up (with an
/// `Interrupted` error, never a hash) once `cancel` is set.
pub(crate) fn full_hash(path: &str, cancel: &AtomicBool) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

fn fingerprint(path: &str, size: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE as usize];
    if size <= SAMPLE * 3 {
        let mut all = Vec::with_capacity(size as usize);
        f.read_to_end(&mut all)?;
        h.update(&all);
    } else {
        for off in [0, size / 2 - SAMPLE / 2, size - SAMPLE] {
            f.seek(SeekFrom::Start(off))?;
            f.read_exact(&mut buf)?;
            h.update(&buf);
        }
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_hash_stops_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let data: Vec<u8> = (0..9 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let p = p.to_str().unwrap();
        let err = full_hash(p, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        // Read in pieces, the hash is the whole file's, as before.
        let want = blake3::hash(&data).to_hex()[..32].to_string();
        assert_eq!(full_hash(p, &AtomicBool::new(false)).unwrap(), want);
    }
}

//! "What was I doing in November last year?" — files grouped by the month
//! they were last changed, newest month first, with the kinds that made up
//! each month so the timeline reads like a diary at a glance.

use crate::index::{FileRecord, Index};
use crate::kind::Kind;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct KindCount {
    pub kind: Kind,
    pub files: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthStat {
    /// "2025-11", in the Mac's local time zone.
    pub month: String,
    pub files: i64,
    pub bytes: i64,
    /// Biggest kinds this month by file count, at most four.
    pub kinds: Vec<KindCount>,
}

const MONTH: &str = "strftime('%Y-%m', f.mtime, 'unixepoch', 'localtime')";

fn valid_month(m: &str) -> bool {
    let b = m.as_bytes();
    b.len() == 7 && b[4] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || c.is_ascii_digit())
}

impl Index {
    /// Months that have files, newest first (at most `limit` months).
    pub fn timeline(&self, limit: usize) -> Result<Vec<MonthStat>> {
        let conn = self.conn();
        // Files dated in the future (bad camera clocks) or before the Mac era
        // would only add noise at either end of the timeline.
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MONTH} AS m, f.kind, count(*), coalesce(sum(f.size),0) FROM files f
             WHERE f.mtime > 631152000 AND f.mtime < strftime('%s','now') + 86400
             GROUP BY m, f.kind ORDER BY m DESC"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
        })?;
        let mut out: Vec<MonthStat> = Vec::new();
        for row in rows.flatten() {
            let (month, kind, files, bytes) = row;
            if out.last().map(|m| m.month != month).unwrap_or(true) {
                if out.len() == limit {
                    break;
                }
                out.push(MonthStat { month, files: 0, bytes: 0, kinds: Vec::new() });
            }
            let m = out.last_mut().unwrap();
            m.files += files;
            m.bytes += bytes;
            if let Some(kind) = Kind::from_i64(kind) {
                m.kinds.push(KindCount { kind, files });
            }
        }
        for m in &mut out {
            m.kinds.sort_by(|a, b| b.files.cmp(&a.files));
            m.kinds.truncate(4);
        }
        Ok(out)
    }

    /// One month's files, newest first; `kind` narrows to one kind.
    pub fn month(&self, month: &str, kind: Option<Kind>, limit: usize, offset: usize) -> Result<Vec<FileRecord>> {
        if !valid_month(month) {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {} FROM files f WHERE {MONTH} = ?1 AND (?2 < 0 OR f.kind = ?2)
             ORDER BY f.mtime DESC LIMIT ?3 OFFSET ?4",
            FileRecord::COLS
        ))?;
        let k = kind.map(|k| k.as_i64()).unwrap_or(-1);
        let v = stmt
            .query_map(params![month, k, limit as i64, offset as i64], FileRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn month_format() {
        assert!(super::valid_month("2025-11"));
        assert!(!super::valid_month("2025-1"));
        assert!(!super::valid_month("2025-11' OR 1"));
    }
}

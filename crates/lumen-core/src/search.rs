//! Search: free text (names, pinyin, folders) plus inline filters.
//!
//! Query language, all optional and combinable:
//!   合同 2024            words, in any order, all must match (pinyin works: "hetong", "ht")
//!   kind:video  类型:视频  only one kind (also: doc, music, pdf, photo, ebook, zip…)
//!   ext:pdf               one extension
//!   in:Downloads          folder path contains
//!   >100mb  <2gb          size bounds (kb, mb, gb)
//!   after:2024-03 before:2025   modified date bounds (YYYY, YYYY-MM, YYYY-MM-DD)
//!   "exact phrase"        quoted text kept together
//!   name:合同             match the name only, never the words inside files
//!
//! Every word matches a file's name, pinyin or folder, or, for documents Lumen
//! has read, the text inside it.

use crate::index::{FileRecord, Index};
use crate::kind::Kind;
use anyhow::Result;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Relevance,
    Recent,
    Oldest,
    Largest,
    Name,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchRequest {
    pub query: String,
    /// Kinds chosen with the UI chips; combined with any kind: in the text.
    pub kinds: Vec<Kind>,
    pub sort: Sort,
    pub limit: usize,
    pub offset: usize,
    /// Match file names and folders only, not the words inside documents.
    pub names_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<FileRecord>,
    pub total: i64,
    /// How many matches each kind has, ignoring the kind filter: drives the
    /// counts on the filter chips.
    pub kind_counts: BTreeMap<String, i64>,
    pub took_us: u64,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Parsed {
    pub terms: Vec<String>,
    /// Terms that must match the name (name:…), never the content.
    pub name_terms: Vec<String>,
    pub kinds: Vec<Kind>,
    pub exts: Vec<String>,
    pub folders: Vec<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub after: Option<i64>,
    pub before: Option<i64>,
}

pub(crate) fn parse(q: &str) -> Parsed {
    let mut p = Parsed::default();
    for tok in tokenize(q) {
        let (quoted, tok) = match tok {
            Tok::Quoted(s) => (true, s),
            Tok::Word(s) => (false, s),
        };
        if quoted {
            if !tok.is_empty() {
                p.terms.push(tok);
            }
            continue;
        }
        let lower = tok.to_lowercase();
        if let Some((key, val)) = lower.split_once([':', '：']) {
            if !val.is_empty() {
                match key {
                    "kind" | "type" | "类型" | "is" => {
                        if let Some(k) = Kind::parse(val) {
                            p.kinds.push(k);
                            continue;
                        }
                    }
                    "name" | "名" | "文件名" => {
                        let orig = tok.split_once([':', '：']).map(|x| x.1).unwrap_or(val).to_string();
                        p.name_terms.push(orig);
                        continue;
                    }
                    "ext" | "后缀" => {
                        p.exts.push(val.trim_start_matches('.').to_string());
                        continue;
                    }
                    "in" | "folder" | "在" => {
                        // keep original case for display, LIKE is case-insensitive anyway
                        p.folders.push(tok.split_once([':', '：']).map(|x| x.1).unwrap_or(val).to_string());
                        continue;
                    }
                    "after" | "since" | "之后" => {
                        if let Some(t) = parse_date(val, false) {
                            p.after = Some(t);
                            continue;
                        }
                    }
                    "before" | "until" | "之前" => {
                        if let Some(t) = parse_date(val, true) {
                            p.before = Some(t);
                            continue;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(rest) = lower.strip_prefix('>') {
            if let Some(n) = parse_size(rest) {
                p.min_size = Some(n);
                continue;
            }
        }
        if let Some(rest) = lower.strip_prefix('<') {
            if let Some(n) = parse_size(rest) {
                p.max_size = Some(n);
                continue;
            }
        }
        p.terms.push(tok);
    }
    p
}

enum Tok {
    Word(String),
    Quoted(String),
}

fn tokenize(q: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in q.chars() {
        match c {
            '"' | '“' | '”' => {
                if in_quote {
                    out.push(Tok::Quoted(std::mem::take(&mut cur)));
                } else if !cur.is_empty() {
                    out.push(Tok::Word(std::mem::take(&mut cur)));
                }
                in_quote = !in_quote;
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(Tok::Word(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(if in_quote { Tok::Quoted(cur) } else { Tok::Word(cur) });
    }
    out
}

fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches('=');
    let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    let n: f64 = num.parse().ok()?;
    let mul = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((n * mul) as i64)
}

/// YYYY, YYYY-MM or YYYY-MM-DD (also with / or .), local-agnostic UTC days.
/// `end` gives the first second after the period, for exclusive upper bounds.
fn parse_date(s: &str, end: bool) -> Option<i64> {
    let parts: Vec<&str> = s.split(['-', '/', '.']).collect();
    let y: i64 = parts.first()?.parse().ok()?;
    if !(1970..=2100).contains(&y) {
        return None;
    }
    let m: i64 = parts.get(1).map(|v| v.parse().ok()).unwrap_or(Some(1))?;
    let d: i64 = parts.get(2).map(|v| v.parse().ok()).unwrap_or(Some(1))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let start = days_from_civil(y, m, d) * 86_400;
    if !end {
        return Some(start);
    }
    Some(match parts.len() {
        1 => days_from_civil(y + 1, 1, 1) * 86_400,
        2 => {
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            days_from_civil(ny, nm, 1) * 86_400
        }
        _ => start + 86_400,
    })
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('%');
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('%');
    o
}

fn fts_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

struct Where {
    sql: Vec<String>,
    args: Vec<Value>,
}

/// Files whose text contains each two-character CJK word, found once per
/// search and reused by the result, count and facet queries. A substring scan
/// is linear in the stored text, so it stops after a few thousand hits:
/// common words fill that quickly, and a list that long gets refined by
/// typing more anyway.
fn short_content_hits(conn: &rusqlite::Connection, p: &Parsed) -> Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    let mut stmt = conn.prepare_cached("SELECT id FROM content WHERE instr(body, ?1) > 0 LIMIT 3000")?;
    for t in &p.terms {
        if t.is_ascii() || t.chars().count() != 2 || out.contains_key(t) {
            continue;
        }
        let ids: Vec<String> = stmt
            .query_map([t], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|i| i.to_string())
            .collect();
        out.insert(t.clone(), if ids.is_empty() { "-1".into() } else { ids.join(",") });
    }
    Ok(out)
}

fn build_where(
    p: &Parsed,
    extra_kinds: &[Kind],
    with_kinds: bool,
    content: bool,
    short: &std::collections::HashMap<String, String>,
) -> Where {
    let mut w = Where { sql: Vec::new(), args: Vec::new() };
    let terms = p.terms.iter().map(|t| (t, content)).chain(p.name_terms.iter().map(|t| (t, false)));
    for (t, content) in terms {
        let mut ors: Vec<String> = Vec::new();
        let long = t.chars().count() >= 3;
        // The trigram tokenizer needs three characters; shorter words (very
        // common in Chinese: 简历, 合同) fall back to LIKE on the name.
        if long {
            ors.push("f.id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)".into());
            w.args.push(Value::Text(fts_quote(t)));
        } else {
            ors.push("f.name LIKE ? ESCAPE '\\' OR f.name_py LIKE ? ESCAPE '\\'".into());
            let pat = like_escape(t);
            w.args.push(Value::Text(pat.clone()));
            w.args.push(Value::Text(pat));
        }
        if content {
            if long {
                ors.push("f.id IN (SELECT rowid FROM content_fts WHERE content_fts MATCH ?)".into());
                w.args.push(Value::Text(fts_quote(t)));
            } else if let Some(ids) = short.get(t) {
                // Two Chinese characters: ids from one substring scan (integer
                // literals only). One or two Latin letters would match nearly
                // every document, so those stay name-only.
                ors.push(format!("f.id IN ({ids})"));
            }
        }
        w.sql.push(format!("({})", ors.join(" OR ")));
    }
    if with_kinds {
        let mut kinds: Vec<Kind> = p.kinds.clone();
        kinds.extend_from_slice(extra_kinds);
        kinds.sort_by_key(|k| k.as_i64());
        kinds.dedup();
        if !kinds.is_empty() {
            let marks = vec!["?"; kinds.len()].join(",");
            w.sql.push(format!("f.kind IN ({marks})"));
            w.args.extend(kinds.iter().map(|k| Value::Integer(k.as_i64())));
        }
    }
    if !p.exts.is_empty() {
        let marks = vec!["?"; p.exts.len()].join(",");
        w.sql.push(format!("f.ext IN ({marks})"));
        w.args.extend(p.exts.iter().map(|e| Value::Text(e.clone())));
    }
    for folder in &p.folders {
        w.sql.push("f.parent LIKE ? ESCAPE '\\'".into());
        w.args.push(Value::Text(like_escape(folder)));
    }
    if let Some(n) = p.min_size {
        w.sql.push("f.size >= ?".into());
        w.args.push(Value::Integer(n));
    }
    if let Some(n) = p.max_size {
        w.sql.push("f.size <= ?".into());
        w.args.push(Value::Integer(n));
    }
    if let Some(t) = p.after {
        w.sql.push("f.mtime >= ?".into());
        w.args.push(Value::Integer(t));
    }
    if let Some(t) = p.before {
        w.sql.push("f.mtime < ?".into());
        w.args.push(Value::Integer(t));
    }
    w
}

impl Index {
    pub fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let started = std::time::Instant::now();
        let p = parse(&req.query);
        let limit = if req.limit == 0 { 60 } else { req.limit.min(500) };
        let conn = self.conn();

        let content = !req.names_only;
        let short = if content { short_content_hits(&conn, &p)? } else { Default::default() };
        let w = build_where(&p, &req.kinds, true, content, &short);
        let where_sql = if w.sql.is_empty() { "1".to_string() } else { w.sql.join(" AND ") };

        // Relevance: exact name > name starts with > name contains > pinyin
        // > folder-only match, then the newest first.
        let mut order_args: Vec<Value> = Vec::new();
        let order = match req.sort {
            Sort::Relevance if !p.terms.is_empty() || !p.name_terms.is_empty() => {
                let all: Vec<String> = p.terms.iter().chain(p.name_terms.iter()).cloned().collect();
                let q = all.join(" ");
                let first = all[0].clone();
                order_args.push(Value::Text(q.clone()));
                order_args.push(Value::Text(format!("{}%", first.replace(['%', '_'], ""))));
                order_args.push(Value::Text(format!("%{}%", first.replace(['%', '_'], ""))));
                order_args.push(Value::Text(format!("%{}%", first.replace(['%', '_'], ""))));
                // A name or folder hit always outranks a match inside the text.
                order_args.push(Value::Text(format!("%{}%", first.replace(['%', '_'], ""))));
                "CASE
                    WHEN lower(substr(f.name, 1, length(f.name) - length(f.ext) - 1)) = lower(?) THEN 0
                    WHEN f.name LIKE ? THEN 1
                    WHEN f.name LIKE ? THEN 2
                    WHEN f.name_py LIKE ? THEN 3
                    WHEN f.parent LIKE ? THEN 4
                    ELSE 5 END, f.mtime DESC"
                    .to_string()
            }
            Sort::Relevance | Sort::Recent => "f.mtime DESC".into(),
            Sort::Oldest => "f.mtime ASC".into(),
            Sort::Largest => "f.size DESC".into(),
            Sort::Name => "f.name COLLATE NOCASE ASC".into(),
        };

        let sql = format!(
            "SELECT {} FROM files f WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {}",
            FileRecord::COLS,
            req.offset
        );
        let mut args = w.args.clone();
        args.extend(order_args);
        let mut stmt = conn.prepare(&sql)?;
        let mut hits = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), FileRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if content && !p.terms.is_empty() {
            add_snippets(&conn, &mut hits, &p.terms)?;
        }

        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM files f WHERE {where_sql}"),
            rusqlite::params_from_iter(w.args.iter()),
            |r| r.get(0),
        )?;

        let wk = build_where(&p, &[], false, content, &short);
        let wk_sql = if wk.sql.is_empty() { "1".to_string() } else { wk.sql.join(" AND ") };
        let mut kind_counts = BTreeMap::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT f.kind, count(*) FROM files f WHERE {wk_sql} GROUP BY f.kind"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(wk.args.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            if let Some(k) = Kind::from_i64(row.0) {
                kind_counts.insert(k.slug().to_string(), row.1);
            }
        }

        Ok(SearchResponse { hits, total, kind_counts, took_us: started.elapsed().as_micros() as u64 })
    }
}

/// For hits whose name does not show why they matched, an excerpt of the
/// text around the first matching word.
fn add_snippets(conn: &rusqlite::Connection, hits: &mut [FileRecord], terms: &[String]) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT body FROM content WHERE id = ?1")?;
    for h in hits.iter_mut() {
        let name = h.name.to_lowercase();
        if terms.iter().all(|t| name.contains(&t.to_lowercase())) {
            continue;
        }
        let body: Option<String> = stmt.query_row([h.id], |r| r.get(0)).ok();
        if let Some(body) = body {
            h.snippet = crate::content::snippet(&body, terms, 28);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filters() {
        let p = parse("合同 kind:pdf >10mb in:Downloads after:2024-03 \"q3 report\"");
        assert_eq!(p.terms, vec!["合同".to_string(), "q3 report".to_string()]);
        assert_eq!(p.kinds, vec![Kind::Pdf]);
        assert_eq!(p.min_size, Some(10 * 1024 * 1024));
        assert_eq!(p.folders, vec!["Downloads".to_string()]);
        assert_eq!(p.after, Some(days_from_civil(2024, 3, 1) * 86_400));
        let p = parse("类型:视频 before:2023");
        assert_eq!(p.kinds, vec![Kind::Video]);
        assert_eq!(p.before, Some(days_from_civil(2024, 1, 1) * 86_400));
        // unknown key stays as text
        assert_eq!(parse("foo:bar").terms, vec!["foo:bar".to_string()]);
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }
}

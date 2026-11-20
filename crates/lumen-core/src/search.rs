//! Search: free text (names, pinyin, folders) plus inline filters.
//!
//! Query language, all optional and combinable:
//!
//! ```text
//! 合同 2024            words, in any order, all must match (pinyin works: "hetong", "ht")
//! kind:video  类型:视频  only one kind (also: doc, music, pdf, photo, ebook, zip…)
//! ext:pdf               one extension
//! in:Downloads          folder path contains (in:"Application Support" for spaces)
//! >100mb  <2gb          size bounds (kb, mb, gb)
//! after:2024-03 before:2025   modified date bounds (YYYY, YYYY-MM, YYYY-MM-DD)
//! "exact phrase"        quoted text kept together
//! name:合同             match the name only, never the words inside files
//! cloud:dropbox 云盘:百度  synced by one service (icloud, dropbox, onedrive,
//!                       gdrive, box, baidu, nutstore, or the start of any
//!                       ~/Library/CloudStorage folder name); cloud:any, cloud:none
//! ```
//!
//! Every word matches a file's name, pinyin or folder, or, for documents Lumen
//! has read, the text inside it. Accents do not matter in names (v0.9):
//! "resume" finds "Résumé.png" and "café" finds "Cafe.txt".

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
    /// Provider ids from cloud:… (see `config::provider_alias`).
    pub clouds: Vec<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub after: Option<i64>,
    pub before: Option<i64>,
}

pub(crate) fn parse(q: &str) -> Parsed {
    let mut p = Parsed::default();
    // Names and text are stored precomposed: a query pasted in decomposed
    // form (copied from a file name) must match them too.
    let q = crate::nfc(q);
    for tok in tokenize(&q) {
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
                    "cloud" | "云盘" | "雲碟" | "云" | "雲" => {
                        p.clouds.push(crate::config::provider_alias(val));
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
    let mut keyed = false;
    for c in q.chars() {
        match c {
            '"' | '“' | '”' => {
                if in_quote {
                    let t = std::mem::take(&mut cur);
                    out.push(if keyed { Tok::Word(t) } else { Tok::Quoted(t) });
                    keyed = false;
                } else if cur.ends_with([':', '：']) {
                    // in:"Application Support", name:"q3 report": the quoted
                    // text is the filter's value, spaces and all. Before, the
                    // key was left as a bare word ("in:") that matched nothing.
                    keyed = true;
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
        out.push(if in_quote && !keyed { Tok::Quoted(cur) } else { Tok::Word(cur) });
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
    // FTS5 reads a NUL as the end of the query: "a\0b" was a syntax error.
    format!("\"{}\"", s.replace('\0', "").replace('"', "\"\""))
}

/// `s` as literal text inside a LIKE pattern.
fn like_lit(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Path patterns of the files one provider syncs (see `config::provider_of`,
/// which this mirrors).
fn provider_patterns(id: &str) -> Vec<String> {
    let storage = |name: &str| {
        let n = like_lit(name);
        [format!("%/Library/CloudStorage/{n}/%"), format!("%/Library/CloudStorage/{n}-%"), format!("%/Library/CloudStorage/{n} %")]
    };
    let mut out = Vec::new();
    if id == "icloud" || id == "any" {
        out.push("%/Library/Mobile Documents/com~apple~CloudDocs/%".to_string());
    }
    if id == "any" {
        out.push("%/Library/CloudStorage/%".to_string());
    }
    let home = crate::index::home().map(|h| like_lit(&h.to_string_lossy()));
    for d in crate::config::PROVIDERS.iter().filter(|d| id == "any" || d.id == id) {
        if id != "any" {
            for s in d.storage {
                out.extend(storage(s));
            }
        }
        if let Some(home) = &home {
            out.extend(d.home.iter().map(|h| format!("{home}/{}/%", like_lit(h))));
        }
    }
    if out.is_empty() {
        // Any other File Provider app, by the start of its folder's name.
        out.push(format!("%/Library/CloudStorage/{}%", like_lit(id)));
    }
    out
}

fn cloud_where(id: &str) -> (String, Vec<Value>) {
    let pats = provider_patterns(if id == "none" { "any" } else { id });
    let any = vec!["f.path LIKE ? ESCAPE '\\'"; pats.len()].join(" OR ");
    let sql = if id == "none" { format!("NOT ({any})") } else { format!("({any})") };
    (sql, pats.into_iter().map(Value::Text).collect())
}

struct Where {
    sql: Vec<String>,
    args: Vec<Value>,
}

fn build_where(
    p: &Parsed,
    extra_kinds: &[Kind],
    with_kinds: bool,
    content: bool,
) -> Where {
    let mut w = Where { sql: Vec::new(), args: Vec::new() };
    let terms = p.terms.iter().map(|t| (t, content)).chain(p.name_terms.iter().map(|t| (t, false)));
    for (t, content) in terms {
        let mut ors: Vec<String> = Vec::new();
        // A word with accents also matches as typed without them ("résumé"
        // finds "resume.pdf"); names with accents carry their folded spelling
        // in name_py, so "resume" finds "Résumé.png" (v0.9).
        let folded = crate::fold(t);
        let mut forms = vec![t.as_str()];
        if crate::has_accents(t) {
            forms.push(folded.as_str());
        }
        for t in forms {
            let long = t.chars().count() >= 3;
            // The trigram tokenizer needs three characters; shorter words (very
            // common in Chinese: 简历, 合同) fall back to LIKE on the name.
            if long {
                ors.push("f.id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)".into());
                w.args.push(Value::Text(fts_quote(t)));
            } else {
                // A word with 汉字 in it is never in name_py whole only when
                // it has no Latin letters either: name_py holds pinyin and
                // folded accented names. Skip the second scan for pure CJK.
                let pat = like_escape(t);
                if !t.chars().all(crate::cjk::is_cjk) {
                    ors.push("f.name LIKE ? ESCAPE '\\' OR f.name_py LIKE ? ESCAPE '\\'".into());
                    w.args.push(Value::Text(pat.clone()));
                } else {
                    ors.push("f.name LIKE ? ESCAPE '\\'".into());
                }
                w.args.push(Value::Text(pat));
            }
        }
        if content {
            if let Some(expr) = crate::cjk::match_expr(t) {
                ors.push("f.id IN (SELECT rowid FROM content_fts WHERE content_fts MATCH ?)".into());
                w.args.push(Value::Text(expr));
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
    for c in &p.clouds {
        let (sql, args) = cloud_where(c);
        w.sql.push(sql);
        w.args.extend(args);
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
        let content = !req.names_only;
        let mut kinds: Vec<Kind> = p.kinds.clone();
        kinds.extend_from_slice(&req.kinds);
        kinds.sort_by_key(|k| k.as_i64());
        kinds.dedup();

        let conn = self.conn();
        let has_words = !p.terms.is_empty() || !p.name_terms.is_empty();
        let (mut hits, total, kind_counts) = if has_words {
            ranked(&conn, &p, &kinds, req.sort, limit, req.offset, content)?
        } else {
            browse(&conn, &p, &kinds, req.sort, limit, req.offset)?
        };
        if content && !p.terms.is_empty() {
            add_snippets(&conn, &mut hits, &p.terms)?;
        }
        Ok(SearchResponse { hits, total, kind_counts, took_us: started.elapsed().as_micros() as u64 })
    }
}

type Found = (Vec<FileRecord>, i64, BTreeMap<String, i64>);

fn kind_map(counts: impl IntoIterator<Item = (i64, i64)>) -> BTreeMap<String, i64> {
    counts
        .into_iter()
        .filter_map(|(k, n)| Kind::from_i64(k).map(|k| (k.slug().to_string(), n)))
        .collect()
}

/// Searches with words. Matching a word means scanning names (or the
/// full-text index), so this makes that pass once: it reads every match's
/// sort keys and kind, counts the kinds, ranks in memory, and only then
/// loads the page of records it shows. Before v0.8 the same scan ran three
/// times (page, total, chip counts), which at 300 000 files cost ~0.7 s.
fn ranked(
    conn: &rusqlite::Connection,
    p: &Parsed,
    kinds: &[Kind],
    sort: Sort,
    limit: usize,
    offset: usize,
    content: bool,
) -> Result<Found> {
    // Chip counts ignore the kind filter, so the pass does too and the kind
    // filter is applied to its rows below.
    let w = build_where(p, &[], false, content);
    // With words in play, one pass over the table beats walking the mtime or
    // size index: a bound like after:2024 still covers a third of the files,
    // and each index hit would be a random row read plus a LIKE. The unary +
    // keeps SQLite off those indexes (538 ms → see tests/scale.rs).
    let w_sql: Vec<String> =
        w.sql.iter().map(|c| c.replace("f.mtime ", "+f.mtime ").replace("f.size ", "+f.size ")).collect();
    let where_sql = if w_sql.is_empty() { "1".to_string() } else { w_sql.join(" AND ") };
    // The ranking expressions sit in the SELECT list, so their values bind
    // before the WHERE clause's.
    let mut args: Vec<Value> = Vec::new();

    // Relevance: exact name > name starts with > name contains > pinyin
    // > folder-only match > words inside. Within each tier, files the user
    // opened from Lumen in the last 30 days come first (most opened, then
    // most recent), then the newest.
    let all: Vec<String> = p.terms.iter().chain(p.name_terms.iter()).cloned().collect();
    // `%` and `_` in the word are literal: escaped, not deleted (deleting
    // them made "report_final" rank as "reportfinal", which no name matches).
    let contains = like_escape(&all[0]);
    let starts = format!("{}%", &contains[1..contains.len() - 1]);
    // The word without accents, against names without them: the folded
    // name is name_py's part before its tab (see `crate::name_key`), so
    // "resume" ranks "Résumé.png" as a name match, not a pinyin one.
    let folded = like_lit(&crate::fold(&all[0]));
    let tier = if matches!(sort, Sort::Relevance) {
        args.push(Value::Text(all.join(" ")));
        args.push(Value::Text(starts));
        args.push(Value::Text(format!("{folded}%\t%")));
        args.push(Value::Text(contains.clone()));
        args.push(Value::Text(format!("%{folded}%\t%")));
        args.push(Value::Text(format!("%{folded}%")));
        args.push(Value::Text(contains.clone()));
        args.push(Value::Text(contains));
        args.push(Value::Integer(crate::index::now() - 30 * 86_400));
        // A name or folder hit always outranks a match inside the text.
        "CASE
            WHEN lower(substr(f.name, 1, length(f.name) - length(f.ext) - 1)) = lower(?) THEN 0
            WHEN f.name LIKE ? ESCAPE '\\' OR f.name_py LIKE ? ESCAPE '\\' THEN 1
            WHEN f.name LIKE ? ESCAPE '\\' OR f.name_py LIKE ? ESCAPE '\\' OR f.name LIKE ? ESCAPE '\\' THEN 2
            WHEN f.name_py LIKE ? ESCAPE '\\' THEN 3
            WHEN f.parent LIKE ? ESCAPE '\\' THEN 4
            ELSE 5 END,
         CASE WHEN o.last >= ? THEN o.count ELSE 0 END, coalesce(o.last, 0)"
    } else {
        "0, 0, 0"
    };
    args.extend(w.args.iter().cloned());
    let name = if matches!(sort, Sort::Name) { "f.name" } else { "''" };
    let sql = format!(
        "SELECT f.id, f.kind, {tier}, f.mtime, f.size, {name}
         FROM files f LEFT JOIN opens o ON o.id = f.id WHERE {where_sql}"
    );
    struct Row {
        id: i64,
        tier: i64,
        opened: i64,
        last: i64,
        mtime: i64,
        size: i64,
        name: String,
    }
    let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
    let mut rows: Vec<Row> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut q = stmt.query(rusqlite::params_from_iter(args.iter()))?;
        while let Some(r) = q.next()? {
            let kind: i64 = r.get(1)?;
            *counts.entry(kind).or_default() += 1;
            if !kinds.is_empty() && !kinds.iter().any(|k| k.as_i64() == kind) {
                continue;
            }
            rows.push(Row {
                id: r.get(0)?,
                tier: r.get(2)?,
                opened: r.get(3)?,
                last: r.get(4)?,
                mtime: r.get(5)?,
                size: r.get(6)?,
                name: if matches!(sort, Sort::Name) { r.get::<_, String>(7)?.to_lowercase() } else { String::new() },
            });
        }
    }
    let total = rows.len() as i64;
    // offset comes from the app's JSON: usize::MAX must not overflow.
    let end = offset.saturating_add(limit).min(rows.len());
    if offset < end {
        let cmp = |a: &Row, b: &Row| match sort {
            Sort::Relevance => a
                .tier
                .cmp(&b.tier)
                .then(b.opened.cmp(&a.opened))
                .then(b.last.cmp(&a.last))
                .then(b.mtime.cmp(&a.mtime)),
            Sort::Recent => b.mtime.cmp(&a.mtime),
            Sort::Oldest => a.mtime.cmp(&b.mtime),
            Sort::Largest => b.size.cmp(&a.size),
            Sort::Name => a.name.cmp(&b.name),
        }
        .then(a.id.cmp(&b.id));
        // Only the page needs to be in order.
        if end < rows.len() {
            rows.select_nth_unstable_by(end - 1, cmp);
            rows.truncate(end);
        }
        rows.sort_unstable_by(cmp);
    }
    let ids: Vec<i64> = rows.get(offset..end).unwrap_or(&[]).iter().map(|r| r.id).collect();
    let hits = records_by_id(conn, &ids)?;
    Ok((hits, total, kind_map(counts)))
}

/// Filters without words (`kind:pdf >10mb`, or nothing at all): the indexes
/// on kind, mtime and size answer these without a scan.
fn browse(conn: &rusqlite::Connection, p: &Parsed, kinds: &[Kind], sort: Sort, limit: usize, offset: usize) -> Result<Found> {
    let w = build_where(p, kinds, true, false);
    let where_sql = if w.sql.is_empty() { "1".to_string() } else { w.sql.join(" AND ") };
    let order = match sort {
        Sort::Relevance | Sort::Recent => "f.mtime DESC",
        Sort::Oldest => "f.mtime ASC",
        Sort::Largest => "f.size DESC",
        Sort::Name => "f.name COLLATE NOCASE ASC",
    };
    // SQLite's LIMIT/OFFSET are i64: a larger usize would not parse.
    let (limit, offset) = (limit.min(i64::MAX as usize), offset.min(i64::MAX as usize));
    let sql = format!(
        "SELECT {} FROM files f WHERE {where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}",
        FileRecord::COLS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let hits = stmt
        .query_map(rusqlite::params_from_iter(w.args.iter()), FileRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let wk = build_where(p, &[], false, false);
    let wk_sql = if wk.sql.is_empty() { "1".to_string() } else { wk.sql.join(" AND ") };
    let mut stmt = conn.prepare_cached(&format!("SELECT f.kind, count(*) FROM files f WHERE {wk_sql} GROUP BY f.kind"))?;
    let counts: Vec<(i64, i64)> = stmt
        .query_map(rusqlite::params_from_iter(wk.args.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // The total is the chip counts summed over the chosen kinds.
    let total = counts
        .iter()
        .filter(|(k, _)| kinds.is_empty() || kinds.iter().any(|x| x.as_i64() == *k))
        .map(|(_, n)| n)
        .sum();
    Ok((hits, total, kind_map(counts)))
}

/// Records for these ids, in this order.
fn records_by_id(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<FileRecord>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!("SELECT {} FROM files f WHERE f.id IN ({marks})", FileRecord::COLS))?;
    let mut by_id: std::collections::HashMap<i64, FileRecord> = stmt
        .query_map(rusqlite::params_from_iter(ids.iter()), FileRecord::from_row)?
        .filter_map(|r| r.ok())
        .map(|r| (r.id, r))
        .collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// For hits whose name does not show why they matched, an excerpt of the
/// text around the first matching word.
fn add_snippets(conn: &rusqlite::Connection, hits: &mut [FileRecord], terms: &[String]) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT body, ocr FROM content WHERE id = ?1")?;
    for h in hits.iter_mut() {
        let name = crate::fold(&h.name);
        if terms.iter().all(|t| name.contains(&crate::fold(t))) {
            continue;
        }
        let row: Option<(String, bool)> = stmt.query_row([h.id], |r| Ok((r.get(0)?, r.get(1)?))).ok();
        if let Some((body, ocr)) = row {
            h.snippet = crate::content::snippet(&body, terms, 28);
            h.ocr = ocr && h.snippet.is_some();
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
        let p = parse("cloud:Dropbox 云盘:百度网盘 cloud:GoogleDrive cloud:pCloud 报告");
        assert_eq!(p.clouds, vec!["dropbox", "baidu", "gdrive", "pcloud"]);
        assert_eq!(p.terms, vec!["报告".to_string()]);
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }
}

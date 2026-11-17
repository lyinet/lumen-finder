//! Full-text ("正文") search: pull the words out of documents so a search for
//! a phrase finds the file that contains it, not only the file named after it.
//!
//! Extraction is local and pure Rust:
//!   plain text      txt md markdown org rst tex csv tsv rtf html
//!   Office Open XML docx pptx xlsx (and OpenDocument odt odp ods)
//!   EPUB            the book's XHTML chapters
//!   PDF             the text layer (scanned PDFs need OCR, a later version)
//!
//! Indexing runs after every scan and after watcher batches, and only touches
//! files whose size or mtime moved since their text was last read, so it is
//! effectively free once the first pass is done.

use crate::index::Index;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Files larger than this are skipped: a 500 MB PDF is a scan, not prose.
pub const MAX_FILE_BYTES: i64 = 80 * 1024 * 1024;
/// Text kept per file. Plenty to find a phrase, small enough that the
/// full-text index stays a fraction of the documents themselves.
pub const MAX_TEXT_CHARS: usize = 120_000;

/// Extensions Lumen can read the words of.
pub const EXTS: &[&str] = &[
    "txt", "md", "markdown", "org", "rst", "tex", "csv", "tsv", "rtf", "html", "htm", "docx",
    "pptx", "xlsx", "odt", "odp", "ods", "epub", "pdf",
];

/// Images Lumen reads with OCR when a helper is set (see `set_ocr_helper`).
pub const OCR_EXTS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "bmp"];
/// Images smaller than this are icons and thumbnails, larger are rarely text.
const OCR_MIN_BYTES: i64 = 20 * 1024;
const OCR_MAX_BYTES: i64 = 40 * 1024 * 1024;
/// A PDF with fewer characters than this in its text layer is a scan.
const SCANNED_PDF_CHARS: usize = 40;

pub fn supported(ext: &str) -> bool {
    EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

static OCR_HELPER: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

/// Point Lumen at the `lumen-ocr` helper (macOS Vision) to read text in images
/// and scanned PDFs, or pass None to turn OCR off.
pub fn set_ocr_helper(path: Option<std::path::PathBuf>) {
    *OCR_HELPER.write().unwrap() = path.filter(|p| p.exists());
}

fn ocr_helper() -> Option<std::path::PathBuf> {
    OCR_HELPER.read().unwrap().clone()
}

/// Run the helper with a time limit; a stuck file never stalls the queue.
fn ocr(path: &Path) -> Result<String> {
    use std::process::{Command, Stdio};
    let Some(bin) = ocr_helper() else { return Ok(String::new()) };
    let mut cmd = Command::new(bin);
    cmd.arg(path).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pace::background_command(&mut cmd);
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            let buf = reader.join().unwrap_or_default();
            anyhow::ensure!(status.success(), "ocr failed");
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("ocr timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentProgress {
    pub done: u64,
    pub total: u64,
    pub current: String,
    pub finished: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentReport {
    pub read: u64,
    pub with_text: u64,
    pub failed: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

/// Read every document whose text is missing or out of date.
pub fn index_content(
    index: &Index,
    cancel: Arc<AtomicBool>,
    on_progress: impl FnMut(&ContentProgress),
) -> Result<ContentReport> {
    let mut conn = index.writer()?;
    index_pending(&mut conn, &cancel, true, on_progress)
}

struct Job {
    id: i64,
    path: String,
    ext: String,
    size: i64,
    mtime: i64,
}

pub(crate) fn index_pending(
    conn: &mut Connection,
    cancel: &Arc<AtomicBool>,
    // Background passes wait for power and Pause (see pace.rs); the watcher's
    // few just-saved files do not.
    paced: bool,
    mut on_progress: impl FnMut(&ContentProgress),
) -> Result<ContentReport> {
    let started = Instant::now();
    if ocr_helper().is_some() {
        // First run with OCR: scans read before had an empty text layer;
        // forget those so they go through the helper.
        let done: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key = 'ocr_v1'", [], |r| r.get(0)).ok();
        if done.is_none() {
            conn.execute_batch(
                "DELETE FROM content WHERE length(body) < 40 AND id IN (SELECT id FROM files WHERE ext = 'pdf');
                 INSERT OR REPLACE INTO meta(key, value) VALUES ('ocr_v1', '1');",
            )?;
        }
    }
    let marks = EXTS.iter().map(|e| format!("'{e}'")).collect::<Vec<_>>().join(",");
    let ocr_marks = OCR_EXTS.iter().map(|e| format!("'{e}'")).collect::<Vec<_>>().join(",");
    // Images join the queue only with OCR on, after the documents (they are slower).
    let images = if ocr_helper().is_some() {
        format!("OR (f.ext IN ({ocr_marks}) AND f.size BETWEEN {OCR_MIN_BYTES} AND {OCR_MAX_BYTES})")
    } else {
        String::new()
    };
    // The watcher reads only what was just saved; the rest of the queue is
    // the background pass's, which may be holding for power.
    let recent = if paced { String::new() } else { format!("AND f.mtime >= {}", crate::index::now() - 3600) };
    let jobs: Vec<Job> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT f.id, f.path, f.ext, f.size, f.mtime FROM files f
             LEFT JOIN content c ON c.id = f.id
             WHERE ((f.ext IN ({marks}) AND f.size <= ?1) {images})
               AND f.cloud = 0 {recent}
               AND (c.id IS NULL OR c.mtime <> f.mtime OR c.size <> f.size)
             ORDER BY f.ext IN ({ocr_marks}), f.mtime DESC"
        ))?;
        let rows = stmt.query_map([MAX_FILE_BYTES], |r| {
            Ok(Job { id: r.get(0)?, path: r.get(1)?, ext: r.get(2)?, size: r.get(3)?, mtime: r.get(4)? })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let total = jobs.len() as u64;
    let mut report = ContentReport { read: 0, with_text: 0, failed: 0, elapsed_ms: 0, cancelled: false };
    if jobs.is_empty() {
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        on_progress(&ContentProgress { done: 0, total: 0, current: String::new(), finished: true });
        return Ok(report);
    }

    // Extract on worker threads (PDFs are CPU-bound), write on this one.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 6);
    let (job_tx, job_rx) = crossbeam_channel::bounded::<Job>(threads * 4);
    let (out_tx, out_rx) = crossbeam_channel::bounded::<(Job, Option<String>, bool)>(threads * 4);
    let mut workers = Vec::new();
    for _ in 0..threads {
        let rx = job_rx.clone();
        let tx = out_tx.clone();
        let cancel = cancel.clone();
        workers.push(std::thread::spawn(move || {
            if paced {
                crate::pace::background_thread();
            }
            for job in rx {
                // Hold still on battery, in Low Power Mode or when paused.
                if cancel.load(Ordering::Relaxed) || (paced && !crate::pace::wait(&cancel)) {
                    break;
                }
                let (text, failed) = match extract(Path::new(&job.path), &job.ext) {
                    Ok(t) => (t, false),
                    Err(_) => (None, true),
                };
                if tx.send((job, text, failed)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(out_tx);
    let feeder = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            for j in jobs {
                if cancel.load(Ordering::Relaxed) || job_tx.send(j).is_err() {
                    break;
                }
            }
        })
    };

    let mut pending: Vec<(Job, Option<String>, bool)> = Vec::new();
    let mut last_tick = Instant::now() - Duration::from_secs(1);
    let mut current = String::new();
    for item in out_rx.iter() {
        report.read += 1;
        if item.2 {
            report.failed += 1;
        }
        if item.1.as_deref().map(|t| !t.is_empty()).unwrap_or(false) {
            report.with_text += 1;
        }
        current.clone_from(&item.0.path);
        pending.push(item);
        if pending.len() >= 64 {
            write(conn, &pending)?;
            pending.clear();
        }
        if last_tick.elapsed() >= Duration::from_millis(150) {
            last_tick = Instant::now();
            on_progress(&ContentProgress { done: report.read, total, current: current.clone(), finished: false });
        }
    }
    write(conn, &pending)?;
    let _ = feeder.join();
    for w in workers {
        let _ = w.join();
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&ContentProgress { done: report.read, total, current: String::new(), finished: true });
    Ok(report)
}

fn write(conn: &mut Connection, items: &[(Job, Option<String>, bool)]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO content (id, mtime, size, body) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET mtime = excluded.mtime, size = excluded.size, body = excluded.body",
        )?;
        let mut fts_del = tx.prepare_cached("DELETE FROM content_fts WHERE rowid = ?1")?;
        let mut fts_ins = tx.prepare_cached("INSERT INTO content_fts(rowid, toks) VALUES (?1, ?2)")?;
        for (job, text, _) in items {
            // The file may have left the index while it was being read.
            let exists: bool = tx
                .query_row("SELECT 1 FROM files WHERE id = ?1", [job.id], |_| Ok(true))
                .unwrap_or(false);
            if exists {
                let body = text.as_deref().unwrap_or("");
                stmt.execute(params![job.id, job.mtime, job.size, body])?;
                fts_del.execute([job.id])?;
                if !body.is_empty() {
                    fts_ins.execute(params![job.id, crate::cjk::index_text(body)])?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}

// ------------------------------------------------------------ extraction --

/// The words of one file, or None when it has none Lumen can read.
pub fn extract(path: &Path, ext: &str) -> Result<Option<String>> {
    let ext = ext.to_ascii_lowercase();
    let raw = match ext.as_str() {
        "txt" | "md" | "markdown" | "org" | "rst" | "tex" | "csv" | "tsv" => read_text(path)?,
        "rtf" => strip_rtf(&read_text(path)?),
        "html" | "htm" => strip_markup(&read_text(path)?),
        "docx" => zip_xml(path, |n| n == "word/document.xml" || n.starts_with("word/footnotes"))?,
        "pptx" => zip_xml(path, |n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))?,
        "xlsx" => zip_xml(path, |n| n == "xl/sharedStrings.xml")?,
        "odt" | "odp" | "ods" => zip_xml(path, |n| n == "content.xml")?,
        "epub" => zip_xml(path, |n| {
            let n = n.to_ascii_lowercase();
            (n.ends_with(".xhtml") || n.ends_with(".html") || n.ends_with(".htm")) && !n.contains("toc")
        })?,
        "pdf" => {
            let t = pdf_text(path).unwrap_or_default();
            if t.trim().chars().count() < SCANNED_PDF_CHARS && ocr_helper().is_some() {
                ocr(path)?
            } else {
                t
            }
        }
        e if OCR_EXTS.contains(&e) => ocr(path)?,
        _ => return Ok(None),
    };
    let text = normalize(&raw);
    Ok(if text.is_empty() { None } else { Some(text) })
}

fn read_text(path: &Path) -> Result<String> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take((MAX_TEXT_CHARS * 4) as u64).read_to_end(&mut buf)?;
    Ok(decode(&buf))
}

/// UTF-8 first, then UTF-16 with a BOM, then GB18030 (old Chinese Windows
/// files), whichever reads cleanly.
fn decode(buf: &[u8]) -> String {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&buf[3..]).into_owned();
    }
    if buf.starts_with(&[0xFF, 0xFE]) || buf.starts_with(&[0xFE, 0xFF]) {
        let le = buf[0] == 0xFF;
        let units: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_string(),
        // Cut mid-character by `take`: keep the valid prefix.
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
        Err(_) => {
            let (s, _, bad) = encoding_rs::GB18030.decode(buf);
            if bad {
                String::from_utf8_lossy(buf).into_owned()
            } else {
                s.into_owned()
            }
        }
    }
}

fn zip_xml(path: &Path, want: impl Fn(&str) -> bool) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut names: Vec<String> = zip.file_names().filter(|n| want(n)).map(String::from).collect();
    // slide2 before slide10
    names.sort_by_key(|n| {
        let digits: String = n.chars().filter(|c| c.is_ascii_digit()).collect();
        (digits.len(), digits, n.clone())
    });
    let mut out = String::new();
    for n in names {
        let mut f = zip.by_name(&n)?;
        let mut xml = String::new();
        // Guard against zip bombs: no part of a document needs more than this.
        (&mut f).take(32 * 1024 * 1024).read_to_string(&mut xml)?;
        out.push_str(&strip_markup(&xml));
        out.push('\n');
        if out.len() > MAX_TEXT_CHARS * 4 {
            break;
        }
    }
    Ok(out)
}

fn pdf_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    // pdf-extract panics on some malformed files; a bad PDF must never take
    // the indexer down with it.
    let r = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&bytes));
    match r {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(anyhow::anyhow!("pdf: {e}")),
        Err(_) => Err(anyhow::anyhow!("pdf: parser panicked")),
    }
}

/// Drop tags, turn block ends into line breaks, decode the common entities.
pub(crate) fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len() / 2);
    let mut chars = s.char_indices().peekable();
    let mut skip_until: Option<&str> = None;
    while let Some((i, c)) = chars.next() {
        if let Some(end) = skip_until {
            if s[i..].starts_with(end) {
                skip_until = None; // fall through: the closing tag is skipped below
            } else {
                continue;
            }
        }
        if c == '<' {
            let rest = &s[i..];
            let lower: String = rest.chars().take(8).collect::<String>().to_ascii_lowercase();
            if lower.starts_with("<script") {
                skip_until = Some("</script");
                continue;
            }
            if lower.starts_with("<style") {
                skip_until = Some("</style");
                continue;
            }
            let close = match rest.find('>') {
                Some(j) => j,
                None => break,
            };
            let tag = &rest[1..close];
            let name = tag.trim_start_matches('/').split([' ', '/', '\t', '\n']).next().unwrap_or("");
            let local = name.rsplit(':').next().unwrap_or(name).to_ascii_lowercase();
            if tag.starts_with('/') && matches!(local.as_str(), "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "si" | "br" | "td" | "row") {
                out.push('\n');
            } else if matches!(local.as_str(), "br" | "tab" | "c" | "s") {
                out.push(' ');
            }
            // skip to after '>'
            let target = i + close;
            while let Some(&(j, _)) = chars.peek() {
                if j > target {
                    break;
                }
                chars.next();
            }
            continue;
        }
        if c == '&' {
            let rest = &s[i..];
            if let Some(semi) = rest.bytes().take(10).position(|b| b == b';') {
                let ent = &rest[1..semi];
                let rep = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
                    e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
                    _ => None,
                };
                if let Some(r) = rep {
                    out.push(r);
                    let target = i + semi;
                    while let Some(&(j, _)) = chars.peek() {
                        if j > target {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// Good-enough RTF: drop control words and groups like fonttbl, keep text,
/// decode \'hh bytes and \uN unicode escapes.
pub(crate) fn strip_rtf(s: &str) -> String {
    let mut out = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut depth = 0i32;
    let mut skip_depth: Option<i32> = None;
    let mut pending_bytes: Vec<u8> = Vec::new();
    let flush = |pb: &mut Vec<u8>, out: &mut String| {
        if !pb.is_empty() {
            let (t, _, _) = encoding_rs::GB18030.decode(pb);
            out.push_str(&t);
            pb.clear();
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            '{' => {
                depth += 1;
                i += 1;
            }
            '}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth -= 1;
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= b.len() {
                    break;
                }
                let n = b[i];
                if n == '\'' && i + 2 < b.len() {
                    let hex: String = b[i + 1..i + 3].iter().collect();
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        if skip_depth.is_none() {
                            pending_bytes.push(v);
                        }
                    }
                    i += 3;
                    continue;
                }
                flush(&mut pending_bytes, &mut out);
                if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < b.len() && b[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word: String = b[start..i].iter().collect();
                    let nstart = i;
                    if i < b.len() && (b[i] == '-' || b[i].is_ascii_digit()) {
                        i += 1;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    let num: Option<i32> = b[nstart..i].iter().collect::<String>().parse().ok();
                    if i < b.len() && b[i] == ' ' {
                        i += 1;
                    }
                    match word.as_str() {
                        "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header" | "footer" => {
                            skip_depth.get_or_insert(depth);
                        }
                        "par" | "line" | "row" if skip_depth.is_none() => out.push('\n'),
                        "tab" | "cell" if skip_depth.is_none() => out.push(' '),
                        "u" if skip_depth.is_none() => {
                            if let Some(v) = num {
                                let v = if v < 0 { v + 65536 } else { v } as u32;
                                if let Some(ch) = char::from_u32(v) {
                                    out.push(ch);
                                }
                                // skip the fallback character
                                if i < b.len() && b[i] == '?' {
                                    i += 1;
                                } else if i + 3 < b.len() && b[i] == '\\' && b[i + 1] == '\'' {
                                    i += 4;
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    if n == '*' {
                        skip_depth.get_or_insert(depth);
                    } else if skip_depth.is_none() && matches!(n, '\\' | '{' | '}') {
                        out.push(n);
                    }
                    i += 1;
                }
            }
            '\r' | '\n' => i += 1,
            _ => {
                flush(&mut pending_bytes, &mut out);
                if skip_depth.is_none() {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    flush(&mut pending_bytes, &mut out);
    out
}

/// Collapse runs of whitespace, drop control characters, cap the length.
pub(crate) fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT_CHARS * 3));
    let mut count = 0usize;
    let mut last_space = true;
    for c in s.chars() {
        if count >= MAX_TEXT_CHARS {
            break;
        }
        if c.is_whitespace() || c.is_control() || c == '\u{fffd}' {
            if !last_space {
                out.push(' ');
                count += 1;
                last_space = true;
            }
            continue;
        }
        out.push(c);
        count += 1;
        last_space = false;
    }
    out.trim_end().to_string()
}

/// A short excerpt of `body` around the first of `terms` it contains, for the
/// result row: "…合同期限为 三年，自 2024 年…".
pub fn snippet(body: &str, terms: &[String], radius: usize) -> Option<String> {
    let lower = body.to_lowercase();
    // Lowercasing can change byte offsets for a few scripts; only trust it
    // when lengths agree, otherwise search case-sensitively.
    let folded = lower.len() == body.len();
    let hay = if folded { lower.as_str() } else { body };
    let (pos, len) = terms
        .iter()
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            let needle = if folded { t.to_lowercase() } else { t.clone() };
            hay.find(&needle).map(|p| (p, needle.len()))
        })
        .min_by_key(|(p, _)| *p)?;
    let start = body[..pos].char_indices().rev().nth(radius.saturating_sub(1)).map(|(i, _)| i).unwrap_or(0);
    let end_from = pos + len;
    let end = body[end_from..].char_indices().nth(radius * 2).map(|(i, _)| end_from + i).unwrap_or(body.len());
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(body[start..end].trim());
    if end < body.len() {
        s.push('…');
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_and_entities() {
        let x = "<w:p><w:r><w:t>租赁&amp;合同</w:t></w:r></w:p><w:p><w:t>第二段 &#x4E2D;</w:t></w:p>";
        assert_eq!(normalize(&strip_markup(x)), "租赁&合同 第二段 中");
        let h = "<html><style>p{}</style><p>Hello <b>world</b></p><script>var a</script></html>";
        assert_eq!(normalize(&strip_markup(h)), "Hello world");
    }

    #[test]
    fn rtf() {
        let r = r"{\rtf1\ansi{\fonttbl\f0 Helvetica;}\f0 Hello\par \u20013?\u25991? text}";
        assert_eq!(normalize(&strip_rtf(r)), "Hello 中文 text");
    }

    #[test]
    fn snippets() {
        let body = "这是一份房屋租赁合同，租期三年，月租金 5000 元。";
        let s = snippet(body, &["租期".into()], 4).unwrap();
        assert!(s.contains("租期"), "{s}");
        assert!(s.starts_with('…'));
        assert_eq!(snippet("Quarterly Report", &["report".into()], 50).unwrap(), "Quarterly Report");
        assert!(snippet(body, &["不存在".into()], 4).is_none());
    }

    #[test]
    fn gb18030() {
        let (bytes, _, _) = encoding_rs::GB18030.encode("中文测试");
        assert_eq!(decode(&bytes), "中文测试");
    }
}

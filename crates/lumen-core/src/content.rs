//! Full-text ("正文") search: pull the words out of documents so a search for
//! a phrase finds the file that contains it, not only the file named after it.
//!
//! Extraction is local and pure Rust:
//!   plain text      txt md markdown org rst tex csv tsv rtf html htm xhtml
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
    "txt", "md", "markdown", "org", "rst", "tex", "csv", "tsv", "rtf", "html", "htm", "xhtml", "docx",
    "pptx", "xlsx", "odt", "odp", "ods", "epub", "pdf",
];

/// Images Lumen reads with OCR when a helper is set (see `set_ocr_helper`).
pub const OCR_EXTS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "bmp"];
/// Images smaller than this are icons and thumbnails, larger are rarely text.
const OCR_MIN_BYTES: i64 = 20 * 1024;
const OCR_MAX_BYTES: i64 = 40 * 1024 * 1024;
/// Most documents one watcher batch reads; the rest wait for the next pass.
const WATCHER_MAX_JOBS: usize = 500;
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
/// `cancel` stops it at once: stopping the watcher (or a pass) used to wait
/// for up to a minute per image being read.
fn ocr(path: &Path, cancel: &AtomicBool) -> Result<String> {
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
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("ocr cancelled");
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
    /// Documents this (watcher) pass left for a background pass: more were
    /// waiting than one live batch reads.
    pub left: bool,
}

/// Read every document whose text is missing or out of date.
pub fn index_content(
    index: &Index,
    cancel: Arc<AtomicBool>,
    on_progress: impl FnMut(&ContentProgress),
) -> Result<ContentReport> {
    let mut conn = index.writer()?;
    if !precompose_text_once(&mut conn, &cancel)? {
        return Ok(ContentReport { read: 0, with_text: 0, failed: 0, elapsed_ms: 0, cancelled: true, left: false });
    }
    index_pending(&mut conn, &cancel, true, on_progress)
}

/// Text is stored precomposed since 0.8.2 (see `crate::nfc`), but a document
/// read before keeps its old text until the file changes: decomposed Hangul
/// or accents that typed words never match. Fix it in place once, in
/// batches (each committed, so a pass cut short resumes where it stopped).
/// Returns false when cancelled before the end.
fn precompose_text_once(conn: &mut Connection, cancel: &AtomicBool) -> Result<bool> {
    use crate::index::{get_meta_i64, set_meta};
    const DONE: &str = "nfc_text_v1";
    const AT: &str = "nfc_text_at";
    if get_meta_i64(conn, DONE)?.is_some() {
        return Ok(true);
    }
    let mut at = get_meta_i64(conn, AT)?.unwrap_or(0);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare_cached("SELECT id, body FROM content WHERE id > ?1 ORDER BY id LIMIT 256")?;
            let rows = stmt.query_map([at], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let Some(&(last, _)) = rows.last() else { break };
        let tx = conn.transaction()?;
        {
            let mut put = tx.prepare_cached("UPDATE content SET body = ?2 WHERE id = ?1")?;
            let mut fts_del = tx.prepare_cached("DELETE FROM content_fts WHERE rowid = ?1")?;
            let mut fts_ins = tx.prepare_cached("INSERT INTO content_fts(rowid, toks) VALUES (?1, ?2)")?;
            for (id, body) in &rows {
                if let std::borrow::Cow::Owned(fixed) = crate::nfc(body) {
                    put.execute(params![id, fixed])?;
                    fts_del.execute([id])?;
                    if !fixed.is_empty() {
                        fts_ins.execute(params![id, crate::cjk::index_text(&fixed)])?;
                    }
                }
            }
        }
        set_meta(&tx, AT, last)?;
        tx.commit()?;
        at = last;
    }
    set_meta(conn, DONE, 1)?;
    Ok(true)
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
    // Rows the watcher just wrote count as recent whatever their mtime (a
    // document moved into another folder keeps its old date but is a new
    // row with no words yet). Capped: a big folder moved in is mostly the
    // background pass's.
    let (recent, limit) = if paced {
        (String::new(), String::new())
    } else {
        conn.execute_batch(crate::scanner::TOUCHED)?;
        (
            format!("AND (f.mtime >= {} OR f.id IN (SELECT id FROM temp.lumen_touched))", crate::index::now() - 3600),
            format!("LIMIT {}", WATCHER_MAX_JOBS + 1),
        )
    };
    let mut jobs: Vec<Job> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT f.id, f.path, f.ext, f.size, f.mtime FROM files f
             LEFT JOIN content c ON c.id = f.id
             WHERE ((f.ext IN ({marks}) AND f.size <= ?1) {images})
               AND f.cloud = 0 {recent}
               AND (c.id IS NULL OR c.mtime <> f.mtime OR c.size <> f.size)
             ORDER BY f.ext IN ({ocr_marks}), f.mtime DESC {limit}"
        ))?;
        let rows = stmt.query_map([MAX_FILE_BYTES], |r| {
            Ok(Job { id: r.get(0)?, path: r.get(1)?, ext: r.get(2)?, size: r.get(3)?, mtime: r.get(4)? })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };
    // Over the cap: the rest is a background pass's, which the watcher
    // asks for (`left`). It used to wait for the next scan, often the next
    // launch, out of content search until then.
    let left = !paced && jobs.len() > WATCHER_MAX_JOBS;
    jobs.truncate(if paced { usize::MAX } else { WATCHER_MAX_JOBS });
    if !paced {
        // Picked up (or left to the background pass): either way not this
        // watcher's to read again next batch.
        conn.execute("DELETE FROM temp.lumen_touched", [])?;
    }
    let total = jobs.len() as u64;
    let mut report = ContentReport { read: 0, with_text: 0, failed: 0, elapsed_ms: 0, cancelled: false, left };
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
                let (text, failed) = match extract_cancellable(Path::new(&job.path), &job.ext, &cancel) {
                    Ok(t) => (t, false),
                    Err(_) => (None, true),
                };
                // Cut short: not a failure, and not "no text" either. Leave
                // it unread so the next pass reads it whole.
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
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
            // The file may have left the index while it was being read, and
            // its id may since belong to another file (ids restart after a
            // reset and the highest one is reused after a delete).
            let exists: bool = tx
                .query_row("SELECT 1 FROM files WHERE id = ?1 AND path = ?2", params![job.id, job.path], |_| Ok(true))
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
    extract_cancellable(path, ext, &AtomicBool::new(false))
}

/// `extract`, giving up on a slow OCR read as soon as `cancel` is set.
pub fn extract_cancellable(path: &Path, ext: &str, cancel: &AtomicBool) -> Result<Option<String>> {
    let ext = ext.to_ascii_lowercase();
    let raw = match ext.as_str() {
        "txt" | "md" | "markdown" | "org" | "rst" | "tex" | "csv" | "tsv" => read_text(path)?,
        "rtf" => strip_rtf(&read_text(path)?),
        "html" | "htm" | "xhtml" => strip_markup(&read_text(path)?),
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
                ocr(path, cancel)?
            } else {
                t
            }
        }
        e if OCR_EXTS.contains(&e) => ocr(path, cancel)?,
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
        let units: Vec<u16> = (2..buf.len().saturating_sub(1))
            .step_by(2)
            .map(|i| [buf[i], buf[i + 1]])
            .map(|c| if le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_string(),
        // Cut mid-character by `take`: keep the valid prefix.
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
        Err(_) if mostly_utf8(buf) => String::from_utf8_lossy(buf).into_owned(),
        Err(_) => {
            // The read cap can cut the last (two- or four-byte) character in
            // half; that alone made a whole GBK file fall back to mojibake.
            for cut in 0..4.min(buf.len()) {
                let (s, bad) = encoding_rs::GB18030.decode_without_bom_handling(&buf[..buf.len() - cut]);
                if !bad {
                    return s.into_owned();
                }
            }
            String::from_utf8_lossy(buf).into_owned()
        }
    }
}

/// UTF-8 with a few bad bytes (a pasted Latin-1 character, a damaged
/// sequence): GB18030 accepts most UTF-8 Chinese byte pairs, so trying it
/// first turned such a file into mojibake. Real GBK text is almost never
/// valid UTF-8 beyond its ASCII.
fn mostly_utf8(buf: &[u8]) -> bool {
    let (mut wide, mut bad) = (0usize, 0usize);
    for chunk in buf.utf8_chunks() {
        wide += chunk.valid().chars().filter(|c| !c.is_ascii()).count();
        if !chunk.invalid().is_empty() {
            bad += 1;
        }
    }
    wide >= 4 && bad * 8 <= wide
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
        let mut xml = Vec::new();
        // Guard against zip bombs: no part of a document needs more than this.
        (&mut f).take(32 * 1024 * 1024).read_to_end(&mut xml)?;
        // Lossy: one bad byte (or the cap cutting a character in half) used
        // to fail the whole document.
        out.push_str(&strip_markup(&String::from_utf8_lossy(&xml)));
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
            // HTML tags are case-insensitive: </SCRIPT> ends <SCRIPT>. A
            // case-sensitive match skipped everything after an upper-case
            // script or style block.
            let head = s.as_bytes().get(i..i + end.len());
            if head.is_some_and(|h| h.eq_ignore_ascii_case(end.as_bytes())) {
                skip_until = None; // fall through: the closing tag is skipped below
                if end == "-->" {
                    // Not a tag: step over the rest of "-->" here.
                    chars.next();
                    chars.next();
                    continue;
                }
            } else {
                continue;
            }
        }
        // '<' opens a tag only before a name, '/', '!' or '?' ("a < b", "<5"
        // are text in HTML).
        let opens_tag = |i: usize| {
            s.as_bytes().get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?'))
        };
        if c == '<' && opens_tag(i) {
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
            if lower.starts_with("<!--") {
                // An HTML comment, often old markup ("<!-- <div>draft</div>
                // -->"): its words are not the page's.
                skip_until = Some("-->");
                continue;
            }
            let close = match rest.find('>') {
                Some(j) => j,
                // Never closed: text, not a tag swallowing the rest.
                None => {
                    out.push(c);
                    continue;
                }
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
                    // Common in saved web pages.
                    "rsquo" => Some('\u{2019}'),
                    "lsquo" => Some('\u{2018}'),
                    "rdquo" => Some('\u{201d}'),
                    "ldquo" => Some('\u{201c}'),
                    "mdash" => Some('\u{2014}'),
                    "ndash" => Some('\u{2013}'),
                    "hellip" => Some('\u{2026}'),
                    "copy" => Some('\u{a9}'),
                    e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
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
    // \'hh bytes are in the code page of the font they are set in, else the
    // document's. Always reading them as GB18030 turned TextEdit's \'e9 (é,
    // code page 1252) into nothing; reading every byte in the code page of
    // any Chinese font in the table did the same to a Western font beside it.
    let fallback = rtf_encoding(s);
    let fonts = rtf_font_encodings(s);
    let deff = rtf_number_after(s, "\\deff");
    let mut enc = deff.and_then(|f| fonts.get(&f).copied()).unwrap_or(fallback);
    // Font changes end with the group they were made in.
    let mut enc_stack: Vec<&'static encoding_rs::Encoding> = Vec::new();
    let flush = |pb: &mut Vec<u8>, out: &mut String, enc: &'static encoding_rs::Encoding| {
        if !pb.is_empty() {
            let (t, _) = enc.decode_without_bom_handling(pb);
            out.push_str(&t);
            pb.clear();
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            '{' => {
                flush(&mut pending_bytes, &mut out, enc);
                enc_stack.push(enc);
                depth += 1;
                i += 1;
            }
            '}' => {
                flush(&mut pending_bytes, &mut out, enc);
                if let Some(e) = enc_stack.pop() {
                    enc = e;
                }
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
                flush(&mut pending_bytes, &mut out, enc);
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
                        "f" if skip_depth.is_none() => {
                            if let Some(e) = num.and_then(|f| fonts.get(&(f as u32)).copied()) {
                                enc = e;
                            } else {
                                enc = fallback;
                            }
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
                flush(&mut pending_bytes, &mut out, enc);
                if skip_depth.is_none() {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    flush(&mut pending_bytes, &mut out, enc);
    out
}

/// The RTF header: the font table and code page are at the start.
fn rtf_head(s: &str) -> &str {
    let mut end = s.len().min(64 * 1024);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn rtf_number_after(s: &str, key: &str) -> Option<u32> {
    let head = rtf_head(s);
    let i = head.find(key)? + key.len();
    let digits: String = head[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Each font in the table whose charset names a code page: `\fN … \fcharsetM`.
fn rtf_font_encodings(s: &str) -> std::collections::HashMap<u32, &'static encoding_rs::Encoding> {
    use encoding_rs::*;
    let mut map = std::collections::HashMap::new();
    let head = rtf_head(s);
    let Some(start) = head.find("\\fonttbl") else { return map };
    let ansi = rtf_ansi_encoding(head);
    // Font entries end with ';'; the table ends where the next one does not start.
    for entry in head[start + 8..].split(';').take(512) {
        let Some((fi, _)) = entry.match_indices("\\f").find(|&(k, _)| entry[k + 2..].starts_with(|c: char| c.is_ascii_digit())) else { continue };
        let num: String = entry[fi + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(num) = num.parse::<u32>() else { continue };
        let cs = entry.find("\\fcharset").and_then(|k| {
            entry[k + 9..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()
        });
        let enc = match cs {
            Some(134) => GB18030,
            Some(136) => BIG5,
            Some(128) => SHIFT_JIS,
            Some(129) => EUC_KR,
            Some(222) => WINDOWS_874,
            Some(238) => WINDOWS_1250,
            Some(204) => WINDOWS_1251,
            Some(161) => WINDOWS_1253,
            Some(162) => WINDOWS_1254,
            Some(177) => WINDOWS_1255,
            Some(178) => WINDOWS_1256,
            Some(186) => WINDOWS_1257,
            Some(163) => WINDOWS_1258,
            Some(77) => MACINTOSH,
            // ANSI or "default": the document's \ansicpg, when it names one.
            Some(0 | 1) => match ansi {
                Some(e) => e,
                None => continue,
            },
            _ => continue,
        };
        map.insert(num, enc);
    }
    map
}

/// The code page of an RTF file's \'hh bytes: `\ansicpgN`, or a Chinese,
/// Japanese or Korean font charset when the document uses one. GB18030 when
/// nothing says otherwise (Lumen's long-standing default).
fn rtf_encoding(s: &str) -> &'static encoding_rs::Encoding {
    use encoding_rs::*;
    let head = rtf_head(s);
    // A CJK font in the font table: its bytes are in that script's code page
    // even when \ansicpg says 1252 (Word writes both).
    for (cs, enc) in [("\\fcharset134", GB18030), ("\\fcharset136", BIG5), ("\\fcharset128", SHIFT_JIS), ("\\fcharset129", EUC_KR)] {
        if head.contains(cs) {
            return enc;
        }
    }
    match rtf_ansi_encoding(head) {
        Some(e) => e,
        None if head.contains("\\mac") && !head.contains("\\ansi") => MACINTOSH,
        None => GB18030,
    }
}

/// The code page `\ansicpgN` names, if Lumen knows it.
fn rtf_ansi_encoding(head: &str) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match rtf_number_after(head, "\\ansicpg")? {
        936 | 54936 | 20936 => GB18030,
        950 => BIG5,
        932 => SHIFT_JIS,
        949 => EUC_KR,
        874 => WINDOWS_874,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        10000 => MACINTOSH,
        _ => return None,
    })
}

/// Collapse runs of whitespace, drop control characters, cap the length.
pub(crate) fn normalize(s: &str) -> String {
    // Precomposed, as queries are (a document written on a Mac can hold
    // decomposed accents or Hangul).
    let s = &*crate::nfc(s);
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
    // Lowercasing can change byte offsets for a few characters (İ grows,
    // the Kelvin sign shrinks). Only search the lowercased text when every
    // character keeps its length, so its offsets are the body's: equal total
    // lengths are not enough and sliced the body mid-character.
    let folded = body.chars().all(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() == c.len_utf8());
    let lower = if folded { body.to_lowercase() } else { String::new() };
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
    fn text_read_before_a_reset_never_lands_on_a_new_file_with_the_same_id() {
        // A reading pass (or a watcher that did not stop in time) holds text
        // it read for row 1; meanwhile the index is reset and scanned again,
        // and row 1 is now another file. Its words used to be written there.
        let dir = tempfile::tempdir().unwrap();
        let index = Index::open(dir.path().join("i.db")).unwrap();
        let mk = |p: &str| crate::index::NewFile {
            path: p.into(),
            name: Path::new(p).file_name().unwrap().to_string_lossy().into(),
            name_py: String::new(),
            parent: "/h".into(),
            ext: "txt".into(),
            kind: crate::Kind::Document,
            size: 5,
            mtime: 100,
            ctime: 100,
            cloud: false,
        };
        index.insert_files(&[mk("/h/old.txt")]).unwrap();
        let id: i64 = index.conn().query_row("SELECT id FROM files", [], |r| r.get(0)).unwrap();
        let job = Job { id, path: "/h/old.txt".into(), ext: "txt".into(), size: 5, mtime: 100 };
        index.reset().unwrap();
        index.insert_files(&[mk("/h/new.txt")]).unwrap();
        let id2: i64 = index.conn().query_row("SELECT id FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(id, id2, "ids restart after a reset");
        let mut conn = index.writer().unwrap();
        write(&mut conn, &[(job, Some("secret words".into()), false)]).unwrap();
        assert_eq!(index.content(id2).unwrap().unwrap_or_default(), "");
    }

    #[test]
    fn text_read_by_an_older_lumen_gets_precomposed_once() {
        // 0.8.1 stored a document's words as written: a Mac document holding
        // decomposed Hangul or accents kept them so, and the file is not read
        // again until it changes, so typed (precomposed) words never found
        // it. The first background pass after the upgrade fixes the stored
        // text and its index, keeping the row's history and vector.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af} \u{1106}\u{116e}\u{11ab}\u{1109}\u{1165} Cre\u{301}me";
        std::fs::write(home.join("notes.txt"), nfd).unwrap();
        let index = Index::open(dir.path().join("i.db")).unwrap();
        let cfg = crate::Config { roots: vec![home.clone()], ..Default::default() };
        crate::scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        let hits = |q: &str| {
            index.search(&crate::SearchRequest { query: q.into(), limit: 5, ..Default::default() }).unwrap().hits.len()
        };
        assert_eq!(hits("\u{bb38}\u{c11c}"), 1);
        // Make it look like 0.8.1 left it.
        {
            let conn = index.writer().unwrap();
            conn.execute("UPDATE content SET body = ?1", [nfd]).unwrap();
            conn.execute("DELETE FROM content_fts", []).unwrap();
            conn.execute("INSERT INTO content_fts(rowid, toks) SELECT id, ?1 FROM content", [crate::cjk::index_text(nfd)]).unwrap();
            conn.execute("DELETE FROM meta WHERE key LIKE 'nfc_text%'", []).unwrap();
            conn.execute("INSERT INTO opens(id, count, last) SELECT id, 4, 1 FROM files", []).unwrap();
            conn.execute("INSERT INTO vectors(id, mtime, size, model, vec) SELECT id, mtime, size, 1, x'01' FROM files", []).unwrap();
        }
        assert_eq!(hits("\u{bb38}\u{c11c}"), 0, "set-up: old text is decomposed");
        // A pass cut short leaves the rest for the next one.
        let r = index_content(&index, Arc::new(AtomicBool::new(true)), |_| {}).unwrap();
        assert!(r.cancelled);
        let r = index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(r.read, 0, "fixed in place, not read again");
        assert_eq!(hits("\u{bb38}\u{c11c}"), 1, "typed Hangul finds the old text");
        assert_eq!(hits("\"\u{d55c}\u{ae00} \u{bb38}\u{c11c}\""), 1);
        assert_eq!(hits("cr\u{e8}me"), 1);
        let conn = index.conn();
        let one = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(one("SELECT count(*) FROM content"), 1);
        assert_eq!(one("SELECT count FROM opens"), 4);
        assert_eq!(one("SELECT count(*) FROM vectors"), 1);
        drop(conn);
        // Idempotent.
        index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(hits("\u{bb38}\u{c11c}"), 1);
    }

    #[test]
    fn watcher_reads_a_capped_batch_and_asks_for_the_rest() {
        // A big folder of old documents moved in: the watcher reads the
        // first WATCHER_MAX_JOBS and says more are left; the rest are the
        // next background pass's.
        let dir = tempfile::tempdir().unwrap();
        let moved = dir.path().join("home/Archive");
        std::fs::create_dir_all(&moved).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(400 * 86_400);
        for i in 0..=WATCHER_MAX_JOBS {
            let p = moved.join(format!("note{i}.txt"));
            std::fs::write(&p, format!("archived note {i}")).unwrap();
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        }
        let index = Index::open(dir.path().join("i.db")).unwrap();
        let cfg = crate::Config { roots: vec![dir.path().join("home")], ..Default::default() };
        let mut conn = index.writer().unwrap();
        let (n, _) =
            crate::scanner::apply_paths(&mut conn, std::slice::from_ref(&moved), &cfg.filter(), &cfg.effective_roots()).unwrap();
        assert_eq!(n, WATCHER_MAX_JOBS + 1);
        let stop = Arc::new(AtomicBool::new(false));
        let r = index_pending(&mut conn, &stop, false, |_| {}).unwrap();
        assert_eq!(r.read, WATCHER_MAX_JOBS as u64);
        assert!(r.left, "the rest must be handed to a background pass");
        // The next live batch does not pick them up (they are old)...
        let r = index_pending(&mut conn, &stop, false, |_| {}).unwrap();
        assert_eq!((r.read, r.left), (0, false));
        // ...the background pass does.
        let r = index_content(&index, stop, |_| {}).unwrap();
        assert_eq!((r.read, r.left), (1, false));
    }

    #[test]
    fn markup_edge_entities_and_bare_angle_brackets() {
        // Never panics, whatever follows '&'.
        for x in ["&", "a&", "&#", "&#;", "&#x;", "&#xD800;", "&#99999999;", "&#x110000;", "&#-1;", "&\u{4e2d};", "&#x4e2d", "\u{4e2d}&amp"] {
            let _ = strip_markup(x);
        }
        assert_eq!(strip_markup("&#xD800; &#99999999;"), "&#xD800; &#99999999;");
        // Upper-case X is valid HTML too.
        assert_eq!(strip_markup("&#X4E2D;&#x4e2d;"), "\u{4e2d}\u{4e2d}");
        // A '<' that does not open a tag is text: it used to drop the rest of
        // the page (no '>' after it) or the words up to the next tag.
        assert_eq!(normalize(&strip_markup("<p>if a < b then stop</p><p>more words</p>")), "if a < b then stop more words");
        assert_eq!(normalize(&strip_markup("<p>3 <5 and 1 << 2</p>")), "3 <5 and 1 << 2");
        assert_eq!(normalize(&strip_markup("a <b and no tag after")), "a <b and no tag after");
    }

    #[test]
    fn markup_and_entities() {
        let x = "<w:p><w:r><w:t>租赁&amp;合同</w:t></w:r></w:p><w:p><w:t>第二段 &#x4E2D;</w:t></w:p>";
        assert_eq!(normalize(&strip_markup(x)), "租赁&合同 第二段 中");
        let h = "<html><style>p{}</style><p>Hello <b>world</b></p><script>var a</script></html>";
        assert_eq!(normalize(&strip_markup(h)), "Hello world");
        // Saved web pages: comments (often old markup) and typographic
        // entities.
        let h = "<p>Don&rsquo;t&nbsp;panic</p><!-- <div>old draft</div> a > b --><p>&ldquo;ok&rdquo; &mdash; done</p>";
        assert_eq!(normalize(&strip_markup(h)), "Don\u{2019}t panic \u{201c}ok\u{201d} \u{2014} done");
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

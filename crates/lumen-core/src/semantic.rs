//! Local semantic search: find a file by what it is about ("北海道的雪景视频",
//! "the slides from the funding pitch") rather than by the words in its name.
//!
//! Every file gets one sentence vector built from its name, its folder and the
//! first lines of its text. Vectors come from an [`Embedder`]; on macOS that is
//! the bundled helper (`lumen-ocr embed`), which uses Apple's NaturalLanguage
//! sentence embeddings: on device, no model to download, nothing leaves the
//! Mac. Apple ships one model per language, so a file is embedded with the
//! Chinese model when its text has Chinese in it and with the English model
//! otherwise, and a query is embedded with both.
//!
//! Vectors are stored quantized to i8 (one f32 scale + N bytes), searched by
//! brute force over an in-memory copy: 100k files × 512 dims is ~50 MB and a
//! query is a few tens of milliseconds, with no extra index to keep in sync.

use crate::index::{get_meta_i64, set_meta, FileRecord, Index};
use crate::kind::Kind;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

/// Which of Apple's sentence models a vector came from. Vectors are only
/// comparable within one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Model {
    Zh = 0,
    En = 1,
}

impl Model {
    fn tag(self) -> &'static str {
        match self {
            Model::Zh => "zh",
            Model::En => "en",
        }
    }
    fn from_i64(v: i64) -> Option<Model> {
        match v {
            0 => Some(Model::Zh),
            1 => Some(Model::En),
            _ => None,
        }
    }
    /// The model for a piece of text: Chinese when it has any Han characters.
    pub fn for_text(text: &str) -> Model {
        if text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c)) {
            Model::Zh
        } else {
            Model::En
        }
    }
}

/// Turns sentences into vectors. One call per batch; `None` for a sentence the
/// model could not embed (empty, or that language's model is missing).
pub trait Embedder: Send + Sync {
    fn embed(&self, items: &[(Model, String)]) -> Result<Vec<Option<Vec<f32>>>>;
    /// Which backend each model uses right now (e.g. "sentence:512", or
    /// "none"). When it changes, that model's vectors are rebuilt: vectors
    /// from different backends are not comparable, and a model that just
    /// became available should get a chance at files it skipped before.
    fn signature(&self, _model: Model) -> Option<String> {
        None
    }
    /// Forget any cached signature so the next call looks again.
    fn refresh(&self) {}
}

static EMBEDDER: RwLock<Option<Arc<dyn Embedder>>> = RwLock::new(None);

/// Use this embedder for indexing and queries, or None to turn semantic
/// search off.
pub fn set_embedder(e: Option<Arc<dyn Embedder>>) {
    *EMBEDDER.write().unwrap() = e;
}

/// Use the macOS helper (`lumen-ocr embed`) when it exists.
pub fn set_embed_helper(path: Option<PathBuf>) {
    set_embedder(path.filter(|p| p.exists()).map(|bin| Arc::new(HelperEmbedder { bin, info: Mutex::new(None) }) as Arc<dyn Embedder>));
}

pub fn available() -> bool {
    EMBEDDER.read().unwrap().is_some()
}

fn embedder() -> Option<Arc<dyn Embedder>> {
    EMBEDDER.read().unwrap().clone()
}

/// `lumen-ocr embed`: reads `zh\t<text>` / `en\t<text>` lines on stdin and
/// writes one line per input: the vector as comma-separated floats, or an
/// empty line when it has none.
struct HelperEmbedder {
    bin: PathBuf,
    /// embed-info output, read once per indexing pass (see `refresh`).
    info: Mutex<Option<String>>,
}

impl HelperEmbedder {
    /// `lumen-ocr embed-info`: "zh\t<backend>" lines. This is also when the
    /// helper asks macOS for a missing language model.
    fn info(&self) -> Option<String> {
        let out = std::process::Command::new(&self.bin)
            .arg("embed-info")
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Embedder for HelperEmbedder {
    fn refresh(&self) {
        *self.info.lock().unwrap() = None;
    }
    fn signature(&self, model: Model) -> Option<String> {
        let mut cache = self.info.lock().unwrap();
        if cache.is_none() {
            *cache = self.info();
        }
        let info = cache.as_ref()?;
        info.lines().find_map(|l| l.strip_prefix(model.tag()).and_then(|r| r.strip_prefix('\t'))).map(|s| s.trim().to_string())
    }

    fn embed(&self, items: &[(Model, String)]) -> Result<Vec<Option<Vec<f32>>>> {
        embed_with_helper(&self.bin, items, EMBED_TIMEOUT)
    }
}

/// One batch through the helper may take this long. A helper that hangs
/// (a language model download that never finishes) used to stall semantic
/// indexing for good: nothing could cancel it.
const EMBED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn embed_with_helper(bin: &std::path::Path, items: &[(Model, String)], timeout: std::time::Duration) -> Result<Vec<Option<Vec<f32>>>> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.arg("embed").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::pace::background_command(&mut cmd);
    let mut child = cmd.spawn()?;
    let mut input = String::new();
    for (m, t) in items {
        // One sentence per line: no line or paragraph separators inside it.
        let clean: String =
            t.chars().map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }).collect();
        input.push_str(m.tag());
        input.push('\t');
        input.push_str(clean.trim());
        input.push('\n');
    }
    let mut stdin = child.stdin.take().unwrap();
    // Write on a thread so a full stdout pipe can never deadlock us.
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        // Bytes, not `lines()`: one non-UTF-8 byte failed the whole batch.
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            if tx.send(parse_vec(&String::from_utf8_lossy(&line))).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let mut out = Vec::with_capacity(items.len());
    while out.len() < items.len() {
        match rx.recv_deadline(deadline) {
            Ok(v) => out.push(v),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("embed helper timed out after {} of {}", out.len(), items.len());
            }
        }
    }
    // Every answer is in (or the helper closed its output): give it until
    // the deadline to exit. A helper that answered and then hung used to
    // block indexing here for good.
    if out.len() < items.len() {
        let _ = child.kill();
    } else {
        loop {
            match child.try_wait() {
                Ok(None) if Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    break;
                }
                _ => break,
            }
        }
    }
    let _ = child.wait();
    anyhow::ensure!(out.len() == items.len(), "embed helper answered {} of {}", out.len(), items.len());
    Ok(out)
}

fn parse_vec(line: &str) -> Option<Vec<f32>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // All or nothing: skipping one bad number shifted every dimension after it.
    let v: Vec<f32> = line.split(',').map(|x| x.trim().parse().ok().filter(|f: &f32| f.is_finite())).collect::<Option<_>>()?;
    (v.len() >= 8).then_some(v)
}

// ------------------------------------------------------------ vectors ----

// `!(n > 1e-9)` on purpose: it is also true for NaN, which `n <= 1e-9` is not.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn normalize(v: &mut [f32]) -> bool {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !(n > 1e-9) {
        return false;
    }
    v.iter_mut().for_each(|x| *x /= n);
    true
}

/// Unit vector → 4-byte scale + one i8 per dimension.
pub(crate) fn quantize(v: &[f32]) -> Vec<u8> {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9);
    let scale = max / 127.0;
    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&scale.to_le_bytes());
    out.extend(v.iter().map(|x| ((x / scale).round().clamp(-127.0, 127.0) as i8) as u8));
    out
}

fn dequant_dot(blob: &[u8], q: &[f32]) -> f32 {
    if blob.len() != 4 + q.len() {
        return 0.0;
    }
    let scale = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    let mut s = 0f32;
    for (b, x) in blob[4..].iter().zip(q) {
        s += (*b as i8) as f32 * x;
    }
    s * scale
}

/// The sentence Lumen embeds for a file: a readable name, where it lives and
/// what it starts with.
pub fn describe(name: &str, parent: &str, body: Option<&str>) -> String {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let mut words = String::new();
    let mut prev_lower = false;
    for c in stem.chars() {
        if matches!(c, '_' | '-' | '.' | '+') {
            words.push(' ');
            prev_lower = false;
            continue;
        }
        // "droneShotCoast" → "drone Shot Coast"
        if c.is_uppercase() && prev_lower {
            words.push(' ');
        }
        prev_lower = c.is_lowercase();
        words.push(c);
    }
    let folders: Vec<&str> = parent.rsplit('/').filter(|s| !s.is_empty()).take(2).collect();
    let mut s = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if !folders.is_empty() {
        s.push_str(" · ");
        s.push_str(&folders.into_iter().rev().collect::<Vec<_>>().join(" "));
    }
    if let Some(b) = body {
        let head: String = b.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
        if !head.is_empty() {
            s.push_str(" · ");
            s.push_str(&head);
        }
    }
    s
}

// ------------------------------------------------------------ indexing ----

#[derive(Debug, Clone, Serialize)]
pub struct SemanticProgress {
    pub done: u64,
    pub total: u64,
    pub finished: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticReport {
    pub embedded: u64,
    pub failed: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

const BATCH: usize = 128;

/// Embed every file whose vector is missing or older than the file.
pub fn index_semantic(
    index: &Index,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(&SemanticProgress),
) -> Result<SemanticReport> {
    let started = Instant::now();
    let mut report = SemanticReport { embedded: 0, failed: 0, elapsed_ms: 0, cancelled: false };
    let Some(emb) = embedder() else {
        on_progress(&SemanticProgress { done: 0, total: 0, finished: true });
        return Ok(report);
    };
    let mut conn = index.writer()?;
    emb.refresh();
    for model in [Model::Zh, Model::En] {
        let Some(sig) = emb.signature(model) else { continue };
        let key = format!("vec_sig_{}", model.tag());
        let old: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| r.get(0)).ok();
        if old.as_deref() != Some(sig.as_str()) {
            log::info!("semantic: {} model is now {sig} (was {old:?}); rebuilding its vectors", model.tag());
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM vectors WHERE model = ?1", [model as i64])?;
            set_meta(&tx, &key, &sig)?;
            bump_rev(&tx)?;
            tx.commit()?;
        }
    }
    let jobs: Vec<(i64, i64, i64, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.size, f.mtime, f.name, f.parent, substr(c.body, 1, 600) FROM files f
             LEFT JOIN vectors v ON v.id = f.id
             LEFT JOIN content c ON c.id = f.id AND c.body <> ''
             WHERE v.id IS NULL OR v.mtime <> f.mtime OR v.size <> f.size
             ORDER BY f.mtime DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let total = jobs.len() as u64;
    let mut done = 0u64;
    on_progress(&SemanticProgress { done, total, finished: total == 0 });
    for chunk in jobs.chunks(BATCH) {
        if !crate::pace::wait(&cancel) {
            report.cancelled = true;
            break;
        }
        let items: Vec<(Model, String)> = chunk
            .iter()
            .map(|(_, _, _, name, parent, body)| {
                let text = describe(name, parent, body.as_deref());
                (Model::for_text(&text), text)
            })
            .collect();
        let vecs = match emb.embed(&items) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("embedding failed: {e}");
                report.failed += chunk.len() as u64;
                done += chunk.len() as u64;
                continue;
            }
        };
        let tx = conn.transaction()?;
        {
            let mut put = tx.prepare_cached(
                "INSERT OR REPLACE INTO vectors(id, mtime, size, model, vec) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for ((job, (model, _)), v) in chunk.iter().zip(&items).zip(vecs) {
                // A file with no vector still gets a row (empty blob) so it is
                // not retried on every pass.
                let blob = match v.and_then(|mut v| normalize(&mut v).then_some(v)) {
                    Some(v) => {
                        report.embedded += 1;
                        quantize(&v)
                    }
                    None => {
                        report.failed += 1;
                        Vec::new()
                    }
                };
                put.execute(params![job.0, job.2, job.1, *model as i64, blob])?;
            }
        }
        bump_rev(&tx)?;
        tx.commit()?;
        done += chunk.len() as u64;
        on_progress(&SemanticProgress { done, total, finished: false });
    }
    on_progress(&SemanticProgress { done, total, finished: true });
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

fn bump_rev(conn: &Connection) -> rusqlite::Result<()> {
    let rev = get_meta_i64(conn, "vec_rev")?.unwrap_or(0) + 1;
    set_meta(conn, "vec_rev", rev)
}

/// How many files have a usable vector.
pub fn semantic_count(index: &Index) -> Result<i64> {
    Ok(index.conn().query_row("SELECT count(*) FROM vectors WHERE length(vec) > 0", [], |r| r.get(0))?)
}

// ------------------------------------------------------------- search ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SemanticRequest {
    pub query: String,
    pub kinds: Vec<Kind>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticHit {
    #[serde(flatten)]
    pub file: FileRecord,
    /// Cosine similarity, 0–1.
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticResponse {
    pub hits: Vec<SemanticHit>,
    /// Files with a vector, so the UI can say "still learning your files".
    pub indexed: i64,
    pub took_us: u64,
}

struct Cache {
    /// Index file, vector revision and how many vectors there are: deleting a
    /// file deletes its vector by trigger without a new revision, and a file
    /// added after it can get the same row id.
    key: (PathBuf, i64, i64),
    rows: Vec<(i64, Model, Vec<u8>)>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Below this similarity a match is noise.
const MIN_SCORE: f32 = 0.30;

pub fn semantic_search(index: &Index, req: &SemanticRequest) -> Result<SemanticResponse> {
    let started = Instant::now();
    let limit = if req.limit == 0 { 40 } else { req.limit.min(200) };
    let query = req.query.trim();
    let indexed = semantic_count(index)?;
    let empty = |indexed| SemanticResponse { hits: vec![], indexed, took_us: started.elapsed().as_micros() as u64 };
    let Some(emb) = embedder() else { return Ok(empty(indexed)) };
    if query.is_empty() {
        return Ok(empty(indexed));
    }
    // Both models: a Chinese query can still match an English file name when
    // the English model knows the words, and vice versa.
    let qv = emb.embed(&[(Model::Zh, query.to_string()), (Model::En, query.to_string())])?;
    let mut qs: [Option<Vec<f32>>; 2] = [None, None];
    for (slot, v) in qs.iter_mut().zip(qv) {
        *slot = v.and_then(|mut v| normalize(&mut v).then_some(v));
    }
    // Querying the model of the query's own language counts fully; the other
    // model is a weaker signal.
    let own = Model::for_text(query);

    let rev = get_meta_i64(&index.conn(), "vec_rev")?.unwrap_or(0);
    let key = (index.path().to_path_buf(), rev, indexed);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().map(|c| c.key != key).unwrap_or(true) {
        let conn = index.conn();
        let mut stmt = conn.prepare("SELECT id, model, vec FROM vectors WHERE length(vec) > 0")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, m, v)| Model::from_i64(m).map(|m| (id, m, v)))
            .collect();
        *cache = Some(Cache { key, rows });
    }
    let rows = &cache.as_ref().unwrap().rows;
    let mut scored: Vec<(f32, i64)> = rows
        .iter()
        .filter_map(|(id, m, blob)| {
            let q = qs[*m as usize].as_ref()?;
            let mut s = dequant_dot(blob, q);
            if *m != own {
                s *= 0.85;
            }
            (s >= MIN_SCORE).then_some((s, *id))
        })
        .collect();
    drop(cache);
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut hits = Vec::new();
    for (score, id) in scored {
        if hits.len() >= limit {
            break;
        }
        let Some(file) = index.get(id)? else { continue };
        if !req.kinds.is_empty() && !req.kinds.contains(&file.kind) {
            continue;
        }
        hits.push(SemanticHit { file, score: score.clamp(0.0, 1.0) });
    }
    Ok(SemanticResponse { hits, indexed, took_us: started.elapsed().as_micros() as u64 })
}

/// Drop every vector (when the user turns the feature off, or the model
/// changes after a macOS upgrade).
pub fn clear_vectors(index: &Index) -> Result<()> {
    let conn = index.writer()?;
    conn.execute("DELETE FROM vectors", [])?;
    bump_rev(&conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_files() {
        assert_eq!(
            describe("droneShot_coast-2019.mp4", "/Users/lin/Movies/旅行", None),
            "drone Shot coast 2019 · Movies 旅行"
        );
        assert_eq!(describe("年终总结.key", "/a", Some("今年  营收\n增长")), "年终总结 · a · 今年 营收 增长");
    }

    #[test]
    fn quantized_dot_is_close() {
        let mut a = vec![0.3, -0.2, 0.9, 0.1];
        let mut b = vec![0.25, -0.1, 0.8, 0.3];
        normalize(&mut a);
        normalize(&mut b);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let approx = dequant_dot(&quantize(&a), &b);
        assert!((exact - approx).abs() < 0.01, "{exact} vs {approx}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_helper_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("helper");
        std::fs::write(&bin, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = Instant::now();
        let r = embed_with_helper(&bin, &[(Model::En, "a".into())], std::time::Duration::from_millis(300));
        assert!(r.is_err());
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn helper_answers_are_read_leniently_but_vectors_strictly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("helper");
        // a good vector, a non-UTF-8 line, a vector with a bad number, an empty line
        std::fs::write(&bin, "#!/bin/sh\ncat >/dev/null\nprintf '1,0,0,0,0,0,0,0\\n\\377\\n1,x,0,0,0,0,0,0,0\\n\\n'\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let items: Vec<(Model, String)> = (0..4).map(|i| (Model::En, format!("s{i}"))).collect();
        let v = embed_with_helper(&bin, &items, std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(v.iter().map(|x| x.is_some()).collect::<Vec<_>>(), vec![true, false, false, false]);
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_answers_then_hangs_is_not_waited_for() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("helper");
        std::fs::write(&bin, "#!/bin/sh\ncat >/dev/null\nprintf '1,0,0,0,0,0,0,0\\n'\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = Instant::now();
        let r = embed_with_helper(&bin, &[(Model::En, "a".into())], std::time::Duration::from_millis(500));
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(r.unwrap().len(), 1);
    }

    #[test]
    fn picks_model() {
        assert_eq!(Model::for_text("北海道 snow"), Model::Zh);
        assert_eq!(Model::for_text("drone shot"), Model::En);
    }
}

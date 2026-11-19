//! Regressions: each test pins a bug that once crashed, hung or answered wrong.

use lumen_core::index::NewFile;
use lumen_core::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The embedder is process-wide; tests that set it take turns.
static GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn nf(path: &str, kind: Kind, size: i64, mtime: i64) -> NewFile {
    let p = Path::new(path);
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    NewFile {
        path: path.into(),
        name_py: pinyin_key(&name),
        ext: p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default(),
        name,
        parent: p.parent().unwrap().to_string_lossy().into_owned(),
        kind,
        size,
        mtime,
        ctime: mtime,
        cloud: false,
    }
}

fn names(index: &Index, query: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: query.into(), ..Default::default() }).unwrap();
    r.hits.into_iter().map(|h| h.name).collect()
}

/// Run `f` on a thread and fail (instead of hanging the suite) when it does not finish.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).expect("did not finish in time (deadlock?)")
}

#[test]
fn snippet_survives_case_folding_that_moves_byte_offsets() {
    // "İ" lowercases to 3 bytes (from 2), the Kelvin sign "K" to 1 (from 3):
    // the lowercased text has the same total length but every offset in
    // between is shifted, so slicing the original at them panicked.
    let body = "\u{130}\u{130}合同\u{212a}";
    let s = content::snippet(body, &["合同".to_string()], 4).expect("found");
    assert!(s.contains("合同"), "{s}");
}

#[test]
fn uppercase_script_tags_do_not_swallow_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.html");
    touch(&p, b"<HTML><SCRIPT>var a = 1;</SCRIPT><P>Visible text here</P></HTML>");
    let t = content::extract(&p, "html").unwrap();
    assert_eq!(t.as_deref(), Some("Visible text here"));
}

#[test]
fn docx_with_a_bad_byte_still_reads() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.docx");
    let mut z = zip::ZipWriter::new(fs::File::create(&p).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    z.start_file("word/document.xml", o).unwrap();
    z.write_all(b"<w:p><w:t>Budget approved \xff today</w:t></w:p>").unwrap();
    z.finish().unwrap();
    let t = content::extract(&p, "docx").unwrap().unwrap_or_default();
    assert!(t.contains("Budget approved"), "{t:?}");
}

#[test]
fn cancelling_content_reading_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    for i in 0..400 {
        touch(&home.join(format!("notes/n{i}.txt")), format!("note number {i}").as_bytes());
    }
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let r = within(30, move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        // Cancel as soon as the first document is read, while the feeder is
        // still blocked handing out the rest.
        index_content(&index, cancel, move |_| c2.store(true, Ordering::Relaxed)).unwrap()
    });
    assert!(r.cancelled);
    assert!(r.read < 400);
}

#[test]
fn remove_path_refreshes_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index
        .insert_files(&[nf("/u/a/one.pdf", Kind::Pdf, 10, 1_700_000_000), nf("/u/a/two.pdf", Kind::Pdf, 10, 1_700_000_000)])
        .unwrap();
    let files = |idx: &Index| idx.overview().unwrap().kinds.iter().map(|k| k.files).sum::<i64>();
    assert_eq!(files(&index), 2);
    index.remove_path("/u/a/one.pdf").unwrap();
    assert_eq!(files(&index), 1, "overview must not keep counting a removed file");
    assert_eq!(index.timeline(10).unwrap().iter().map(|m| m.files).sum::<i64>(), 1);
}

#[test]
fn index_from_a_newer_lumen_forgets_old_text() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("a/secret.md"), "pineapple contract terms".as_bytes());
    let db = dir.path().join("index.db");
    {
        let index = Index::open(&db).unwrap();
        let cfg = Config { roots: vec![home.clone()], ..Config::default() };
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        assert_eq!(names(&index, "pineapple"), vec!["secret.md"]);
        index.conn().pragma_update(None, "user_version", 99).unwrap();
    }
    // Opening drops the files table; a different file gets the reused id and
    // must not inherit the old file's words.
    let index = Index::open(&db).unwrap();
    index.insert_files(&[nf("/u/b/other.pdf", Kind::Pdf, 10, 1_700_000_000)]).unwrap();
    assert!(names(&index, "pineapple").is_empty());
}

#[test]
fn huge_offsets_and_limits_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index.insert_files(&[nf("/u/a/report.pdf", Kind::Pdf, 10, 1_700_000_000)]).unwrap();
    index.insert_files(&[nf("/u/a/invoice.pdf", Kind::Pdf, 10, 1_700_000_000)]).unwrap();
    for query in ["report", "kind:pdf", ""] {
        let r = index
            .search(&SearchRequest { query: query.into(), offset: usize::MAX, limit: usize::MAX, ..Default::default() })
            .unwrap();
        assert!(r.hits.is_empty(), "{query}");
        assert!(r.total >= 1, "{query}");
    }
    assert!(index.duplicates(1, usize::MAX).unwrap().is_empty());
    assert!(index.month("2023-11", None, 10, usize::MAX).unwrap().is_empty());
    assert!(index.collection("invoices", 10, usize::MAX).unwrap().is_empty());
}

#[test]
fn underscores_and_percent_rank_literally() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index
        .insert_files(&[
            nf("/u/docs/report_final_v2.pdf", Kind::Pdf, 10, 1_600_000_000),
            // newer, but only "reportfinal"-ish via its folder
            nf("/u/report_final/zzz.pdf", Kind::Pdf, 10, 1_700_000_000),
        ])
        .unwrap();
    assert_eq!(names(&index, "report_final")[0], "report_final_v2.pdf", "a name hit outranks a folder hit");
}

#[test]
fn quoted_filter_values_keep_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index
        .insert_files(&[
            nf("/u/Application Support/notes.md", Kind::Document, 10, 1_700_000_000),
            nf("/u/Other/notes.md", Kind::Document, 10, 1_700_000_000),
            nf("/u/Other/q3 report.pdf", Kind::Pdf, 10, 1_700_000_000),
        ])
        .unwrap();
    assert_eq!(names(&index, "notes in:\"Application Support\""), vec!["notes.md"]);
    assert_eq!(names(&index, "name:\"q3 report\""), vec!["q3 report.pdf"]);
}

#[test]
fn search_text_is_never_fts_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index.insert_files(&[nf("/u/a/AND OR NOT.pdf", Kind::Pdf, 10, 1_700_000_000)]).unwrap();
    for q in [
        "\"", "\"\"", "\"unterminated", "a\"b", "*", "***", "-x", "NEAR(a b)", "a:b", "((", ")", "AND", "OR NOT",
        "^abc", "合同*", "col:val", "{a b}", "'", "%", "_", "\\", "in:", "kind:", ">", "<", ">=10mb", "after:", "before:x",
        "after:2024-02-31", ">1e400gb", "“智能引号”", "name:", "ext:.", "a\u{0}b",
    ] {
        index.search(&SearchRequest { query: q.into(), ..Default::default() }).unwrap_or_else(|e| panic!("{q:?}: {e}"));
    }
    assert_eq!(names(&index, "\"AND OR\""), vec!["AND OR NOT.pdf"]);
}

#[test]
fn config_with_one_bad_field_keeps_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("config.json");
    fs::write(&p, r#"{"roots": ["/data/photos"], "theme": null, "min_size": -5, "include_hidden": true, "future_key": 1}"#)
        .unwrap();
    let c = Config::load(&p);
    assert_eq!(c.roots, vec![std::path::PathBuf::from("/data/photos")]);
    assert!(c.include_hidden);
    assert_eq!(c.theme, "auto", "bad value falls back to the default");
    assert_eq!(c.min_size, 1);
    // A byte-order mark (some editors add one) is not a reason to forget everything.
    fs::write(&p, "\u{feff}{\"include_hidden\": true}").unwrap();
    assert!(Config::load(&p).include_hidden);
    // Garbage still means defaults, never a crash.
    fs::write(&p, "not json").unwrap();
    assert_eq!(Config::load(&p).theme, "auto");
    fs::write(&p, "[1,2]").unwrap();
    assert_eq!(Config::load(&p).theme, "auto");
}

/// Remembers only a "snow" concept, enough to tell two files apart.
struct SnowEmbedder;
impl Embedder for SnowEmbedder {
    fn embed(&self, items: &[(semantic::Model, String)]) -> anyhow::Result<Vec<Option<Vec<f32>>>> {
        Ok(items
            .iter()
            .map(|(_, t)| {
                let snow = if t.to_lowercase().contains("snow") { 1.0 } else { 0.0 };
                Some(vec![snow, 1.0 - snow, 0.0, 0.0, 0.0, 0.0, 0.0, 0.1])
            })
            .collect())
    }
}

#[test]
fn semantic_cache_is_not_reused_after_reset() {
    let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    set_embedder(Some(Arc::new(SnowEmbedder)));
    index.insert_files(&[nf("/u/a/snow day.mov", Kind::Video, 10, 1_700_000_000)]).unwrap();
    index_semantic(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let top = |idx: &Index| {
        semantic_search(idx, &SemanticRequest { query: "snow".into(), ..Default::default() })
            .unwrap()
            .hits
            .into_iter()
            .map(|h| h.file.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(top(&index), vec!["snow day.mov"]);
    index.reset().unwrap();
    // Same row id, different file, same number of embedding passes.
    index.insert_files(&[nf("/u/a/budget.xlsx", Kind::Spreadsheet, 10, 1_700_000_000)]).unwrap();
    index_semantic(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let hits = top(&index);
    set_embedder(None);
    assert!(hits.is_empty(), "stale vectors answered for a new file: {hits:?}");
}

// ---------------------------------------------------------------- round 2 --

fn all_names(index: &Index) -> Vec<String> {
    let mut v: Vec<String> = index
        .conn()
        .prepare("SELECT name FROM files ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn deleted_root_on_the_boot_volume_is_dropped() {
    // A folder the user indexed and then deleted is not an unplugged drive:
    // its files used to stay in search forever, all of them dead links.
    let dir = tempfile::tempdir().unwrap();
    let keep = dir.path().join("keep");
    let gone = dir.path().join("gone");
    touch(&keep.join("a.pdf"), b"a");
    touch(&gone.join("b.pdf"), b"b");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![keep.clone(), gone.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["a.pdf", "b.pdf"]);
    fs::remove_dir_all(&gone).unwrap();
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["a.pdf"]);
    assert_eq!(r.removed, 1);
}

#[cfg(unix)]
#[test]
fn unreadable_folder_keeps_its_files_on_rescan() {
    // Same rule as the watcher: a folder Lumen lost access to (privacy
    // settings, chmod) is not a folder whose files were deleted.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("open/a.pdf"), b"a");
    touch(&home.join("locked/b.pdf"), b"b");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["a.pdf", "b.pdf"]);
    let locked = home.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let denied = fs::read_dir(&locked).is_err();
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {});
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if !denied {
        return; // root: permissions are not enforced
    }
    r.unwrap();
    assert_eq!(all_names(&index), vec!["a.pdf", "b.pdf"]);
    // Really deleted files next to it still go.
    fs::remove_file(home.join("open/a.pdf")).unwrap();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["b.pdf"]);
}

#[test]
fn big_gb18030_text_cut_mid_character_still_reads() {
    // Text files are read up to a byte cap. A GBK/GB18030 novel cut in the
    // middle of a two-byte character failed to decode and was indexed as
    // UTF-8 mojibake: no Chinese word in it could be found.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("novel.txt");
    let text = "第一章 北海道的雪景。".repeat(60_000);
    let (bytes, _, _) = encoding_rs::GB18030.encode(&text);
    // Put the cap in the middle of a character, whatever the cap is.
    let mut bytes = bytes.into_owned();
    bytes.splice(0..0, *b"xx");
    // 2 + 21-byte lines: the 480,000-byte read cap lands on a character's first byte.
    touch(&p, &bytes);
    let t = content::extract(&p, "txt").unwrap().unwrap();
    assert!(t.contains("北海道的雪景"), "{}", &t[..60.min(t.len())]);
}

#[test]
fn rtf_in_a_western_code_page_keeps_accents() {
    // TextEdit writes é as \'e9 under \ansicpg1252; it was decoded as
    // GB18030 and the word was lost.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("menu.rtf");
    touch(&p, br"{\rtf1\ansi\ansicpg1252\cocoartf2761{\fonttbl\f0\fswiss Helvetica;}\f0 Caf\'e9 cr\'e8me br\'fbl\'e9e}");
    assert_eq!(content::extract(&p, "rtf").unwrap().as_deref(), Some("Café crème brûlée"));
    // Chinese Windows RTF still reads as GB18030.
    let (gb, _, _) = encoding_rs::GB18030.encode("合同");
    let hex: String = gb.iter().map(|b| format!("\\'{b:02x}")).collect();
    touch(&p, format!(r"{{\rtf1\ansi\ansicpg936 {hex}}}").as_bytes());
    assert_eq!(content::extract(&p, "rtf").unwrap().as_deref(), Some("合同"));
}

#[test]
fn semantic_cache_drops_a_deleted_files_vector() {
    // Deleting a file deletes its vector (a trigger) without a new vector
    // revision; the next file inserted can reuse the row id, and the cached
    // vector then answered for it.
    let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    set_embedder(Some(Arc::new(SnowEmbedder)));
    index.insert_files(&[nf("/u/a/budget.xlsx", Kind::Spreadsheet, 10, 1_600_000_000)]).unwrap();
    index.insert_files(&[nf("/u/a/snow day.mov", Kind::Video, 10, 1_700_000_000)]).unwrap();
    index_semantic(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let top = |idx: &Index| {
        semantic_search(idx, &SemanticRequest { query: "snow".into(), ..Default::default() })
            .unwrap()
            .hits
            .into_iter()
            .map(|h| h.file.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(top(&index), vec!["snow day.mov"]);
    index.remove_path("/u/a/snow day.mov").unwrap();
    index.insert_files(&[nf("/u/a/taxes.pdf", Kind::Pdf, 10, 1_700_000_000)]).unwrap();
    let hits = top(&index);
    set_embedder(None);
    assert!(hits.is_empty(), "a deleted file's vector answered for another: {hits:?}");
}

// ---------------------------------------------------------------- round 3 --

#[cfg(unix)]
#[test]
fn rescan_next_to_a_locked_folder_still_drops_a_deleted_subfolder() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("locked/b.pdf"), b"b");
    touch(&home.join("sub/deep/c.pdf"), b"c");
    touch(&home.join("lockedness/d.pdf"), b"d"); // shares a prefix with "locked"
    touch(&home.join("e.pdf"), b"e");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["b.pdf", "c.pdf", "d.pdf", "e.pdf"]);
    let locked = home.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let denied = fs::read_dir(&locked).is_err();
    fs::remove_dir_all(home.join("sub")).unwrap();
    fs::remove_dir_all(home.join("lockedness")).unwrap();
    fs::remove_file(home.join("e.pdf")).unwrap();
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {});
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    r.unwrap();
    // Root ignores permissions and sees the locked file too; either way
    // everything that was really deleted is gone.
    assert_eq!(all_names(&index), vec!["b.pdf"], "denied: {denied}");
}

#[cfg(unix)]
#[test]
fn folder_whose_files_cannot_be_looked_at_keeps_them() {
    // A folder that can be listed but not entered (r--): its files' sizes
    // cannot be read. They were dropped as deleted on the next scan, while
    // the watcher (rightly) keeps them.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("shy/b.pdf"), b"b");
    touch(&home.join("a.pdf"), b"a");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let shy = home.join("shy");
    fs::set_permissions(&shy, fs::Permissions::from_mode(0o444)).unwrap();
    let denied = fs::metadata(shy.join("b.pdf")).is_err();
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {});
    fs::set_permissions(&shy, fs::Permissions::from_mode(0o755)).unwrap();
    if !denied {
        return;
    }
    r.unwrap();
    assert_eq!(all_names(&index), vec!["a.pdf", "b.pdf"]);
}

#[cfg(unix)]
#[test]
fn root_replaced_by_a_link_or_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let other = dir.path().join("other");
    touch(&home.join("a.pdf"), b"a");
    touch(&other.join("z.pdf"), b"z");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    // The folder is now a link to another folder: its files are what is there.
    fs::remove_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&other, &home).unwrap();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["z.pdf"]);
    // Now a plain file: nothing that was in the folder is there any more.
    fs::remove_file(&home).unwrap();
    fs::write(&home, b"not a folder").unwrap();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), Vec::<String>::new());
}

#[test]
fn utf8_text_with_one_stray_byte_is_not_read_as_gb18030() {
    // A UTF-8 note with a single Latin-1 byte pasted in (or a cut-off
    // sequence mid-file) failed UTF-8 and was then decoded as GB18030, which
    // accepts most UTF-8 Chinese byte pairs: the whole file became mojibake.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("notes.txt");
    for stray in [0x80u8, 0xA9, 0xE9] {
        let mut bytes = "会议纪要：北海道项目预算".repeat(20).into_bytes();
        bytes.push(b' ');
        bytes.push(stray);
        bytes.extend_from_slice("下周一出发".repeat(20).as_bytes());
        touch(&p, &bytes);
        let t = content::extract(&p, "txt").unwrap().unwrap();
        assert!(t.contains("北海道项目预算") && t.contains("下周一出发"), "stray {stray:#x}: {}", &t[..60.min(t.len())]);
    }
}

#[test]
fn rtf_with_a_chinese_and_a_western_font_reads_both() {
    // Word and TextEdit list a Chinese font in the table and write accented
    // Latin text in a Western font next to it: every \'hh byte was decoded in
    // the Chinese code page, so "Café" lost its é.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mixed.rtf");
    let (gb, _, _) = encoding_rs::GB18030.encode("合同");
    let hex: String = gb.iter().map(|b| format!("\\'{b:02x}")).collect();
    let rtf = format!(
        r"{{\rtf1\ansi\ansicpg1252\deff0{{\fonttbl{{\f0\fswiss\fcharset0 Helvetica;}}{{\f1\fnil\fcharset134 PingFangSC-Regular;}}}}\f0 Caf\'e9 {{\f1 {hex}}} cr\'e8me \f1 {hex}}}"
    );
    touch(&p, rtf.as_bytes());
    assert_eq!(content::extract(&p, "rtf").unwrap().as_deref(), Some("Café 合同 crème 合同"));
}

#[cfg(unix)]
#[test]
fn overlapping_roots_index_each_file_once() {
    // Settings lets a user add ~/Documents and ~/Documents/Work, or a link
    // that points inside another root: every file under both was indexed
    // twice (and a live change updated only one of the copies).
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("Documents");
    let other = dir.path().join("Other");
    touch(&docs.join("Work/plan.pdf"), b"p");
    touch(&docs.join("top.pdf"), b"t");
    touch(&other.join("far.pdf"), b"f");
    let link_in = dir.path().join("WorkLink"); // → Documents/Work
    let link_same = dir.path().join("DocsLink"); // → Documents
    let link_out = docs.join("OtherLink"); // inside Documents, → Other (not walked by it)
    std::os::unix::fs::symlink(docs.join("Work"), &link_in).unwrap();
    std::os::unix::fs::symlink(&docs, &link_same).unwrap();
    std::os::unix::fs::symlink(&other, &link_out).unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config {
        roots: vec![docs.join("Work"), link_in.clone(), docs.clone(), link_same.clone(), link_out.clone(), docs.join("Work/")],
        ..Config::default()
    };
    let roots = cfg.effective_roots();
    assert!(roots.contains(&link_out), "a link inside a root to elsewhere is still its own root: {roots:?}");
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["far.pdf", "plan.pdf", "top.pdf"]);
    // An outer root that is a link, with a plain folder inside its target.
    let cfg = Config { roots: vec![docs.join("Work"), link_same.clone()], ..Config::default() };
    assert_eq!(cfg.effective_roots(), vec![link_same.clone()]);
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(all_names(&index), vec!["plan.pdf", "top.pdf"]);
}

#[test]
fn a_month_lists_every_kind_it_has() {
    // A big month's view counts kinds from the timeline (it only loads the
    // newest 1000 files); kinds past the fourth were dropped there, so their
    // chips counted only what was among those 1000.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    for (i, ext) in ["pdf", "docx", "xlsx", "pptx", "jpg", "mp4"].iter().enumerate() {
        for j in 0..=i {
            touch(&home.join(format!("f{i}_{j}.{ext}")), b"x");
        }
    }
    let index = Index::open(dir.path().join("index.db")).unwrap();
    scan(&index, &Config { roots: vec![home], ..Config::default() }, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let months = index.timeline(24).unwrap();
    assert_eq!(months.len(), 1);
    assert_eq!(months[0].kinds.len(), 6);
    assert_eq!(months[0].kinds.iter().map(|k| k.files).sum::<i64>(), months[0].files);
}

#[test]
fn an_index_from_before_ocr_marks_opens_and_searches() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    drop(Index::open(&db).unwrap());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE content DROP COLUMN ocr;").unwrap();
    drop(conn);
    let index = Index::open(&db).unwrap();
    assert!(index.search(&SearchRequest { query: "x".into(), ..Default::default() }).is_ok());
}

/// An unplugged drive: the root stays in the saved settings, its files stay
/// searchable (marked offline) through rescans, and the
/// mounted roots around it are scanned as usual.
#[test]
fn offline_drive_root_kept_in_settings_and_index() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("a.txt"), b"hello");
    let vol = std::path::PathBuf::from("/Volumes/__lumen_unplugged_drive__");
    assert!(!vol.exists());
    let cfg_path = dir.path().join("config.json");
    let cfg = Config { roots: vec![home.clone(), vol.join("Movies")], ..Config::default() };
    cfg.save(&cfg_path).unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    index.insert_files(&[nf("/Volumes/__lumen_unplugged_drive__/Movies/trip.mp4", Kind::Video, 10, 1_700_000_000)]).unwrap();

    for _ in 0..2 {
        let cfg = Config::load(&cfg_path);
        assert_eq!(cfg.roots, vec![home.clone(), vol.join("Movies")], "the unplugged root is still configured");
        assert_eq!(cfg.effective_roots(), vec![home.clone()], "only walked roots are skipped");
        scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
        cfg.save(&cfg_path).unwrap();
    }
    let r = index.search(&SearchRequest { query: "trip".into(), ..Default::default() }).unwrap();
    assert_eq!(r.hits.len(), 1);
    assert!(r.hits[0].offline);
    assert_eq!(names(&index, "a.txt"), vec!["a.txt"]);
}

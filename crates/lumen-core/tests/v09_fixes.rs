//! 0.9 fixes: scanned PDFs read by OCR before `content.ocr` existed are
//! labelled once, and the first full hashing of duplicates can be stopped.

use lumen_core::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// Text stored for a file as an older Lumen stored it: no OCR mark.
fn store_old_text(db: &Path, name: &str, body: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let id: i64 = conn.query_row("SELECT id FROM files WHERE name = ?1", [name], |r| r.get(0)).unwrap();
    conn.execute("UPDATE content SET body = ?2, ocr = 0 WHERE id = ?1", rusqlite::params![id, body]).unwrap();
    conn.execute("DELETE FROM content_fts WHERE rowid = ?1", [id]).unwrap();
    conn.execute("INSERT INTO content_fts(rowid, toks) VALUES (?1, ?2)", rusqlite::params![id, cjk::index_text(body)])
        .unwrap();
}

#[test]
fn scans_read_by_ocr_before_the_ocr_column_say_in_image() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    // PDFs with no text layer: what Lumen OCRs.
    touch(&home.join("Scans/Scan_0312.pdf"), b"%PDF-1.4 not really a pdf, just an image");
    touch(&home.join("Scans/Scan_0401.pdf"), b"%PDF-1.4 another scan");
    touch(&home.join("notes.md"), "房屋租赁合同 草稿".as_bytes());
    // A PDF with fonts has a text layer: what was stored is that text.
    touch(&home.join("Reports/annual.pdf"), b"%PDF-1.4 << /Type /Font /Subtype /Type1 >> (Annual report)");
    let db = dir.path().join("i.db");
    let index = Index::open(&db).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    // Their words came from OCR in an older Lumen, before the mark existed.
    store_old_text(&db, "Scan_0312.pdf", "房屋租赁合同 出租方 承租方 押金两个月 租期自2025年3月起");
    store_old_text(&db, "Scan_0401.pdf", "租赁合同 补充协议 第二页");
    store_old_text(&db, "annual.pdf", "Annual report revenue grew");
    // One of them changed since: it is read afresh anyway, so left alone.
    fs::File::options()
        .write(true)
        .open(home.join("Scans/Scan_0401.pdf"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("DELETE FROM meta WHERE key IN ('ocr_flag_v1', 'ocr_flag_at')", []).unwrap();
    }
    let ocr_of = |name: &str| -> i64 {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row("SELECT c.ocr FROM content c JOIN files f ON f.id = c.id WHERE f.name = ?1", [name], |r| r.get(0))
            .unwrap()
    };
    let search = |q: &str| index.search(&SearchRequest { query: q.into(), ..Default::default() }).unwrap();
    let r = search("出租方");
    assert!(!r.hits[0].ocr, "precondition: labelled “in text”");

    // A cancelled pass leaves it for the next one.
    index_content(&index, Arc::new(AtomicBool::new(true)), |_| {}).unwrap();
    assert_eq!(index.meta("ocr_flag_v1").unwrap(), None);

    index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(index.meta("ocr_flag_v1").unwrap().as_deref(), Some("1"));
    assert_eq!(ocr_of("Scan_0312.pdf"), 1);
    let r = search("出租方");
    assert_eq!(r.hits[0].name, "Scan_0312.pdf");
    assert!(r.hits[0].ocr, "now “in image”");
    // The markdown note and the PDF with a text layer were read as text and
    // stay that way; the scan that changed since is left to its re-read.
    assert_eq!(ocr_of("notes.md"), 0);
    assert_eq!(ocr_of("annual.pdf"), 0);
    assert_eq!(ocr_of("Scan_0401.pdf"), 0);

    // Once only: text marked otherwise later is not second-guessed.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE content SET ocr = 0", []).unwrap();
    }
    index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(ocr_of("Scan_0312.pdf"), 0);
}

fn nf(path: &Path, size: i64) -> index::NewFile {
    index::NewFile {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        name_py: String::new(),
        parent: path.parent().unwrap().to_string_lossy().into_owned(),
        ext: "img".into(),
        kind: Kind::Archive,
        size,
        mtime: 1_700_000_000,
        ctime: 1_700_000_000,
        cloud: false,
    }
}

#[test]
fn duplicate_hashing_can_be_stopped_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    // Big enough to need a full hash after the samples agree.
    let size = 2 * 1024 * 1024;
    let data = vec![5u8; size];
    let (a, b) = (dir.path().join("a.img"), dir.path().join("b.img"));
    touch(&a, &data);
    touch(&b, &data);
    index.insert_files(&[nf(&a, size as i64), nf(&b, size as i64)]).unwrap();
    let full_hashes = || -> i64 {
        index.conn().query_row("SELECT count(*) FROM files WHERE hash LIKE '%:%'", [], |r| r.get(0)).unwrap()
    };

    let stop = AtomicBool::new(true);
    let t = std::time::Instant::now();
    let groups = index.duplicates_cancellable(1024, 10, &stop).unwrap();
    assert!(groups.is_empty(), "nothing reported once stopped");
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(full_hashes(), 0, "no hash recorded for a read that was cut short");

    let groups = index.duplicates_cancellable(1024, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].files.len(), 2);
    assert_eq!(full_hashes(), 2);
    // The plain call is the same thing, never stopped.
    assert_eq!(index.duplicates(1024, 10).unwrap().len(), 1);
}

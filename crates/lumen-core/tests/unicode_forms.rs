//! macOS hands out the same name in two Unicode forms: precomposed (NFC,
//! what people type) and decomposed (NFD, what older disks and some apps
//! store, and what file-system events can report). The same file must be one
//! row whichever form arrives, and a typed query must find it.
#![cfg(unix)]

use lumen_core::*;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// "Café 报告", "Résumé", "한글" (Hangul splits into letters in NFD).
const CAFE_NFC: &str = "Caf\u{e9} \u{62a5}\u{544a}.txt";
const CAFE_NFD: &str = "Cafe\u{301} \u{62a5}\u{544a}.txt";
const RESUME_NFC: &str = "R\u{e9}sum\u{e9}";
const RESUME_NFD: &str = "Re\u{301}sume\u{301}";
const HANGUL_NFC: &str = "\u{d55c}\u{ae00}.md";
const HANGUL_NFD: &str = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.md";

fn names(index: &Index, q: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: q.into(), limit: 100, ..Default::default() }).unwrap();
    let mut v: Vec<String> = r.hits.into_iter().map(|h| h.name).collect();
    v.sort();
    v
}

#[test]
fn typed_queries_find_decomposed_names_and_words() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(RESUME_NFD)).unwrap();
    fs::write(home.join(RESUME_NFD).join(CAFE_NFD), "Cre\u{301}me bru\u{302}le\u{301}e recipe").unwrap();
    fs::write(home.join(HANGUL_NFD), "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af} notes").unwrap();
    let index = Index::open(dir.path().join("i.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();

    assert_eq!(names(&index, "caf\u{e9}").len(), 1, "NFC query, NFD name");
    assert_eq!(names(&index, "cafe\u{301}").len(), 1, "NFD query, NFD name");
    assert_eq!(names(&index, &format!("in:{RESUME_NFC}")).len(), 1, "NFC folder filter");
    assert_eq!(names(&index, "\u{d55c}\u{ae00}").len(), 1, "Hangul typed, decomposed on disk");
    assert_eq!(names(&index, "\"cr\u{e9}me br\u{fb}l\u{e9}e\"").len(), 1, "NFC words inside an NFD document");
    // The row still opens: its path is the one on disk.
    let r = index.search(&SearchRequest { query: "caf\u{e9}".into(), limit: 5, ..Default::default() }).unwrap();
    assert!(std::path::Path::new(&r.hits[0].path).exists());
    let _ = (CAFE_NFC, HANGUL_NFC);
}

#[test]
fn rows_from_before_get_precomposed_names() {
    // An index written by an older Lumen holds the decomposed name; a rescan
    // keeps existing rows as they are, so it must fix their names once.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(RESUME_NFD)).unwrap();
    let path = home.join(RESUME_NFD).join(CAFE_NFD);
    fs::write(&path, b"x").unwrap();
    let index = Index::open(dir.path().join("i.db")).unwrap();
    let meta = fs::metadata(&path).unwrap();
    index
        .insert_files(&[index::NewFile {
            path: path.to_string_lossy().into(),
            name: CAFE_NFD.into(),
            name_py: String::new(),
            parent: home.join(RESUME_NFD).to_string_lossy().into(),
            ext: "txt".into(),
            kind: Kind::Document,
            size: meta.len() as i64,
            mtime: meta.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
            ctime: 0,
            cloud: false,
        }])
        .unwrap();
    // (Found since 0.9, accents aside, but under its decomposed name.)
    assert_eq!(names(&index, "caf\u{e9}"), vec![CAFE_NFD.to_string()], "precondition: stored decomposed");
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.files, 1);
    assert_eq!(names(&index, "caf\u{e9}"), vec![CAFE_NFC.to_string()]);
    assert_eq!(names(&index, &format!("in:{RESUME_NFC}")).len(), 1);
}

#[test]
fn history_on_a_twin_row_from_before_survives_the_upgrade() {
    // An older Lumen's watcher added a second row for a decomposed file,
    // under the precomposed spelling of its path, and the user opened the
    // file from that row. The upgrade's scan drops the twin (the folder does
    // not list that spelling); the file's open history must stay with it.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(RESUME_NFD)).unwrap();
    let path = home.join(RESUME_NFD).join(CAFE_NFD);
    fs::write(&path, b"x").unwrap();
    let index = Index::open(dir.path().join("i.db")).unwrap();
    let meta = fs::metadata(&path).unwrap();
    let row = |p: &std::path::Path, name: &str| index::NewFile {
        path: p.to_string_lossy().into(),
        name: name.into(),
        name_py: String::new(),
        parent: p.parent().unwrap().to_string_lossy().into(),
        ext: "txt".into(),
        kind: Kind::Document,
        size: meta.len() as i64,
        mtime: meta.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
        ctime: 0,
        cloud: false,
    };
    let twin = home.join(RESUME_NFC).join(CAFE_NFC);
    index.insert_files(&[row(&path, CAFE_NFD), row(&twin, CAFE_NFC)]).unwrap();
    index.record_open(&twin.to_string_lossy()).unwrap();
    index.record_open(&twin.to_string_lossy()).unwrap();
    index.record_open(&path.to_string_lossy()).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(index.file_count().unwrap(), 1);
    let recent = index.recently_opened(10).unwrap();
    assert_eq!(recent.len(), 1, "the open history went with the twin row");
    assert_eq!(recent[0].path, path.to_string_lossy());
    let count: i64 = index.conn().query_row("SELECT count FROM opens", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 3, "both rows' opens, counted once");
}

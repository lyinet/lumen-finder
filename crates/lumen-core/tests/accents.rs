//! Accent-insensitive names (v0.9): "resume" finds "Résumé.png", "cafe"
//! finds "Café", and an accented query finds the plain spelling, while
//! pinyin, CJK and NFC handling stay as they were, and indexes written by
//! an older Lumen are fixed by a one-time background migration.

use lumen_core::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn ranked(index: &Index, q: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: q.into(), limit: 100, ..Default::default() }).unwrap();
    r.hits.into_iter().map(|h| h.name).collect()
}

fn names(index: &Index, q: &str) -> Vec<String> {
    let mut v = ranked(index, q);
    v.sort();
    v
}

fn setup() -> (tempfile::TempDir, Index, Config) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("Jobs/R\u{e9}sum\u{e9}.png"), b"png");
    touch(&home.join("Jobs/resume-old.pdf"), b"pdf");
    // Decomposed on disk, as older macOS disks keep it.
    touch(&home.join("Food/Cafe\u{301} Menu.pdf"), b"pdf");
    touch(&home.join("Food/Stra\u{df}e.txt"), b"txt");
    touch(&home.join("Work/Caf\u{e9}合同.docx"), b"docx");
    touch(&home.join("Work/年终总结.key"), b"key");
    touch(&home.join("Work/がぎ.txt"), b"kana");
    touch(&home.join("Work/かき.txt"), b"kana");
    touch(&home.join("Reports/my resume notes.txt"), b"notes");
    let index = Index::open(dir.path().join("i.db")).unwrap();
    let cfg = Config { roots: vec![home], content_search: false, ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    (dir, index, cfg)
}

#[test]
fn plain_letters_find_accented_names_and_back() {
    let (_dir, index, _) = setup();
    assert_eq!(names(&index, "resume"), vec!["R\u{e9}sum\u{e9}.png", "my resume notes.txt", "resume-old.pdf"]);
    assert_eq!(names(&index, "RESUME kind:image"), vec!["R\u{e9}sum\u{e9}.png"]);
    assert_eq!(names(&index, "cafe"), vec!["Caf\u{e9} Menu.pdf", "Caf\u{e9}合同.docx"]);
    // An accented query finds both spellings.
    assert_eq!(names(&index, "r\u{e9}sum\u{e9}"), vec!["R\u{e9}sum\u{e9}.png", "my resume notes.txt", "resume-old.pdf"]);
    assert_eq!(names(&index, "Caf\u{e9}"), vec!["Caf\u{e9} Menu.pdf", "Caf\u{e9}合同.docx"]);
    // Decomposed query, too short for the trigram index, and spelled-out letters.
    assert_eq!(names(&index, "Cafe\u{301}"), vec!["Caf\u{e9} Menu.pdf", "Caf\u{e9}合同.docx"]);
    assert_eq!(names(&index, "strasse"), vec!["Stra\u{df}e.txt"]);
    assert_eq!(names(&index, "name:resume ext:png"), vec!["R\u{e9}sum\u{e9}.png"]);
}

#[test]
fn accented_names_rank_as_name_matches() {
    let (_dir, index, _) = setup();
    // A name that starts with the word, accents or not, outranks one that
    // only contains it; both outrank folder matches.
    let r = ranked(&index, "resume");
    assert_eq!(r[2], "my resume notes.txt", "{r:?}");
    let r = ranked(&index, "cafe menu");
    assert_eq!(r[0], "Caf\u{e9} Menu.pdf");
}

#[test]
fn pinyin_cjk_and_kana_are_unchanged() {
    let (_dir, index, _) = setup();
    assert_eq!(names(&index, "nzzj"), vec!["年终总结.key"]);
    assert_eq!(names(&index, "nianzhong"), vec!["年终总结.key"]);
    assert_eq!(names(&index, "合同"), vec!["Caf\u{e9}合同.docx"]);
    assert_eq!(names(&index, "hetong"), vec!["Caf\u{e9}合同.docx"]);
    assert_eq!(names(&index, "cafeht"), vec!["Caf\u{e9}合同.docx"]);
    // Voiced kana are different letters, not accents.
    assert_eq!(names(&index, "がぎ"), vec!["がぎ.txt"]);
    assert_eq!(names(&index, "かき"), vec!["かき.txt"]);
    // Suggestions still read pinyin initials (an accented name's key now
    // starts with its folded spelling).
    let s = index.suggestions(5).unwrap();
    assert!(s.iter().any(|s| s.query == "nzzj"), "{s:?}");
    assert!(s.iter().all(|s| s.query.chars().all(|c| c.is_ascii_lowercase())), "{s:?}");
}

#[test]
fn an_older_index_is_folded_once() {
    let (dir, index, cfg) = setup();
    // As an older Lumen wrote it: name_py is pinyin only, and the migration
    // has not run.
    {
        let conn = rusqlite::Connection::open(dir.path().join("i.db")).unwrap();
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT id, name FROM files")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for (id, name) in rows {
            conn.execute("UPDATE files SET name_py = ?2 WHERE id = ?1", rusqlite::params![id, pinyin_key(&name)]).unwrap();
        }
        conn.execute("DELETE FROM meta WHERE key IN ('fold_v1', 'fold_at')", []).unwrap();
    }
    assert!(!names(&index, "resume").contains(&"R\u{e9}sum\u{e9}.png".to_string()), "precondition");
    assert_eq!(names(&index, "nzzj"), vec!["年终总结.key"]);
    // A cancelled scan leaves it for later (and keeps every row).
    let cancelled = Arc::new(AtomicBool::new(true));
    scan(&index, &cfg, cancelled, |_| {}).unwrap();
    assert_eq!(index.meta("fold_v1").unwrap(), None);
    // The next scan (a rescan never rewrites name_py itself) fixes them.
    let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.added + r.removed, 0);
    assert_eq!(index.meta("fold_v1").unwrap().as_deref(), Some("1"));
    assert_eq!(names(&index, "resume"), vec!["R\u{e9}sum\u{e9}.png", "my resume notes.txt", "resume-old.pdf"]);
    assert_eq!(names(&index, "cafeht"), vec!["Caf\u{e9}合同.docx"]);
    assert_eq!(names(&index, "nzzj"), vec!["年终总结.key"]);
}

#[test]
fn snippets_are_not_added_for_an_accent_only_name_match() {
    // "resume" matched the name "Résumé.md": the row shows its folder, not a
    // passage of text the word only also appears in.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("R\u{e9}sum\u{e9}.md"), b"My resume, updated for 2025.");
    let index = Index::open(dir.path().join("i.db")).unwrap();
    let cfg = Config { roots: vec![home], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let r = index.search(&SearchRequest { query: "resume".into(), ..Default::default() }).unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].snippet, None);
}

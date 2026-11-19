//! End-to-end scenarios: a mixed home folder is scanned, read, searched with
//! many query forms, changed under a live watcher, searched again, and reset.
#![cfg(unix)]

use lumen_core::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

fn put(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn set_mtime(p: &Path, secs_ago: u64) {
    let f = fs::File::options().write(true).open(p).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
}

fn docx(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("[Content_Types].xml", o).unwrap();
    z.write_all(b"<Types/>").unwrap();
    z.start_file("word/document.xml", o).unwrap();
    write!(z, "<w:document><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>").unwrap();
    z.finish().unwrap();
}

fn names(index: &Index, query: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: query.into(), limit: 500, ..Default::default() }).unwrap();
    let mut v: Vec<String> = r.hits.into_iter().map(|h| h.name).collect();
    v.sort();
    v
}

fn paths(index: &Index, query: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: query.into(), limit: 500, ..Default::default() }).unwrap();
    r.hits.into_iter().map(|h| h.path).collect()
}

fn v(xs: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = xs.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

struct Tree {
    _dir: tempfile::TempDir,
    home: PathBuf,
    index: Index,
    cfg: Config,
}

fn tree() -> Tree {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    put(&home.join("Documents/notes.txt"), "quarterly budget for the lighthouse project".as_bytes());
    put(&home.join("Documents/readme.md"), b"# Setup\nInstall the zebra toolkit first.");
    put(&home.join("Documents/page.html"), b"<html><body><p>Hello <b>marmalade</b> world</p><script>var hidden='scripted';</script></body></html>");
    put(&home.join("Documents/letter.rtf"), br"{\rtf1\ansi{\fonttbl\f0 Helvetica;}\f0 Dear committee, the \b walrus\b0  arrived.}");
    docx(&home.join("Documents/contract.docx"), "This agreement covers the pelican shipment");
    put(&home.join("Documents/工作/年度报告.txt"), "今年的销售额增长了百分之二十".as_bytes());
    put(&home.join("Documents/工作/会议纪要.md"), "讨论了新产品发布计划 roadmap".as_bytes());
    put(&home.join("Pictures/🎉 party 🎈.jpg"), &[1u8; 3000]);
    let long = format!("{}.txt", "very_long_name_".repeat(16)); // ~240 bytes
    put(&home.join("Documents").join(&long), b"long named file with the word saxophone");
    let mut deep = home.join("Deep");
    for i in 0..30 {
        deep = deep.join(format!("level{i}"));
    }
    put(&deep.join("bottom.md"), b"found at the bottom");
    put(&home.join("Music/song.mp3"), &vec![9u8; 2 * 1024 * 1024]);
    put(&home.join("Movies/clip.MOV"), &vec![3u8; 5000]);
    // Skipped: hidden files and folders, excluded names, *.ext patterns.
    put(&home.join(".secret.txt"), b"hidden");
    put(&home.join(".hidden/visible-inside.txt"), b"hidden folder");
    put(&home.join("build/out.txt"), b"excluded folder name");
    put(&home.join("Documents/debug.log.txt"), b"x");
    put(&home.join("Documents/dump.csv"), b"a,b");
    // A symlink loop must not hang the scan.
    std::os::unix::fs::symlink(&home, home.join("Documents/loop")).unwrap();
    std::os::unix::fs::symlink("..", home.join("Deep/up")).unwrap();
    set_mtime(&home.join("Documents/notes.txt"), 400 * 86_400);
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config {
        roots: vec![home.clone()],
        excluded_names: vec!["build".into(), "*.csv".into()],
        content_search: true,
        ..Config::default()
    };
    Tree { _dir: dir, home, index, cfg }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn scan_read_search_mutate_reset() {
    let t = tree();
    let (index, home) = (&t.index, &t.home);
    let started = Instant::now();
    let r = scan(index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "symlink loop slowed the scan");
    assert!(!r.cancelled);
    let all = names(index, "kind:document");
    assert!(!all.iter().any(|n| n.contains("secret") || n.contains("visible-inside") || n == "out.txt"), "{all:?}");
    assert!(names(index, "dump").is_empty(), "*.csv skipped");
    assert_eq!(r.files, 12, "{:?}", names(index, "after:1970"));

    let c = index_content(index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(c.read, 9, "{c:?}");
    assert_eq!(c.failed, 0);

    // Words inside, in every format.
    assert_eq!(names(index, "lighthouse"), v(&["notes.txt"]));
    assert_eq!(names(index, "zebra"), v(&["readme.md"]));
    // .html is not a kind Lumen lists, so its words are not read either.
    assert!(names(index, "marmalade").is_empty());
    assert_eq!(names(index, "walrus"), v(&["letter.rtf"]));
    assert_eq!(names(index, "pelican"), v(&["contract.docx"]));
    assert_eq!(names(index, "销售额"), v(&["年度报告.txt"]));
    assert_eq!(names(index, "增长"), v(&["年度报告.txt"]));
    assert_eq!(names(index, "发布计划"), v(&["会议纪要.md"]));
    assert_eq!(names(index, "saxophone").len(), 1);
    assert_eq!(names(index, "bottom"), v(&["bottom.md"]));
    // Names: Chinese, pinyin, emoji, long names.
    assert_eq!(names(index, "报告"), v(&["年度报告.txt"]));
    assert_eq!(names(index, "niandu"), v(&["年度报告.txt"]));
    assert_eq!(names(index, "party"), v(&["🎉 party 🎈.jpg"]));
    assert_eq!(names(index, "🎉"), v(&["🎉 party 🎈.jpg"]));
    assert_eq!(names(index, "very_long_name_very").len(), 1);
    // Filters.
    assert_eq!(names(index, "kind:image"), v(&["🎉 party 🎈.jpg"]));
    assert_eq!(names(index, "类型:视频"), v(&["clip.MOV"]));
    assert_eq!(names(index, "ext:mov"), v(&["clip.MOV"]));
    assert_eq!(names(index, "ext:.MOV"), v(&["clip.MOV"]));
    assert_eq!(names(index, "in:工作"), v(&["会议纪要.md", "年度报告.txt"]));
    assert_eq!(names(index, "roadmap in:工作"), v(&["会议纪要.md"]));
    assert_eq!(names(index, ">1mb"), v(&["song.mp3"]));
    assert_eq!(names(index, "<1kb kind:image").len(), 0);
    assert_eq!(names(index, "kind:document <100b ext:md"), v(&["readme.md", "会议纪要.md", "bottom.md"]));
    let year = 1970 + (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() / 31_556_952);
    let old = names(index, &format!("before:{}", year - 1));
    assert_eq!(old, v(&["notes.txt"]), "only the backdated file is older than last year");
    assert!(!names(index, &format!("after:{year}-01")).contains(&"notes.txt".to_string()));
    assert_eq!(names(index, "\"lighthouse project\""), v(&["notes.txt"]));
    assert_eq!(names(index, "budget kind:document in:Documents"), v(&["notes.txt"]));
    assert!(names(index, "budget kind:image").is_empty());
    assert!(names(index, "\"\"").len() <= 12);
    assert!(names(index, "nonexistentword").is_empty());

    // Live changes.
    let (tx, rx) = crossbeam_channel::unbounded();
    let watcher = Watcher::start(index, &t.cfg, move |b| {
        let _ = tx.send(b);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // Rename.
    fs::rename(home.join("Documents/readme.md"), home.join("Documents/guide.md")).unwrap();
    // Move an old document across folders: its words must follow it.
    fs::create_dir_all(home.join("Archive")).unwrap();
    fs::rename(home.join("Documents/notes.txt"), home.join("Archive/notes.txt")).unwrap();
    // Delete.
    fs::remove_file(home.join("Documents/letter.rtf")).unwrap();
    // Modify.
    put(&home.join("Documents/工作/年度报告.txt"), "利润翻倍 profit doubled".as_bytes());
    // Touch only.
    set_mtime(&home.join("Music/song.mp3"), 10);
    // New file in an excluded folder and with an excluded ext.
    put(&home.join("build/new.txt"), b"never");
    put(&home.join("Documents/more.csv"), b"never");
    // Rename a folder.
    fs::rename(home.join("Documents/工作"), home.join("Documents/项目")).unwrap();

    wait_until("guide.md", || names(index, "guide") == v(&["guide.md"]));
    wait_until("the move", || paths(index, "notes").iter().any(|p| p.contains("/Archive/")));
    wait_until("the folder rename", || names(index, "in:项目").len() == 2);
    wait_until("the new words", || names(index, "利润") == v(&["年度报告.txt"]));
    drop(watcher);
    while rx.try_recv().is_ok() {}

    assert!(names(index, "readme").is_empty());
    assert_eq!(names(index, "zebra"), v(&["guide.md"]));
    assert!(names(index, "walrus").is_empty());
    assert!(names(index, "letter").is_empty());
    assert!(names(index, "in:工作").is_empty());
    assert!(names(index, "销售额").is_empty(), "old words left after a change");
    assert_eq!(names(index, "never").len(), 0);
    assert!(names(index, "more").is_empty());
    assert!(names(index, "new").iter().all(|n| n != "new.txt"));
    assert_eq!(paths(index, "lighthouse"), vec![home.join("Archive/notes.txt").to_string_lossy().into_owned()], "moved file lost its words");

    // A rescan agrees with the live updates.
    let r = scan(index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.added + r.removed, 0, "{r:?}");

    // Reset, then build again from nothing.
    index.reset().unwrap();
    assert_eq!(index.file_count().unwrap(), 0);
    assert!(names(index, "zebra").is_empty());
    let r = scan(index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert!(r.full);
    assert_eq!(r.files, 11);
    index_content(index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(names(index, "zebra"), v(&["guide.md"]));
    assert_eq!(names(index, "lighthouse"), v(&["notes.txt"]));
}

#[test]
fn odd_queries_never_fail() {
    let t = tree();
    scan(&t.index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index_content(&t.index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let long = "x".repeat(10_000);
    let odd = [
        "", " ", "kind:", "in:", ">", "<", "\"", "\"unterminated", "ext:", "a\"b", "%", "_", "*", "OR", "AND", "NOT",
        "-foo", "name:", "after:2024-13", "before:abc", "：", "in:\"工 作\"", "🎉🎈", "\0", "a\0b", "NEAR(a b)",
        "col:val", "{x}", "^ab", "(", ")", "'", "\\", ">1xb", ">99999999999999999999tb", "<-1", "after:9999",
        "before:1970", "kind:pdf kind:image", "ext:md ext:txt", "in:Documents in:工作", "\u{200b}", "ab\u{301}c",
        "name:\"🎉 party\"", long.as_str(), "“lighthouse project”", "LIGHTHOUSE", "Lighthouse Budget", "类型：文档",
    ];
    for q in odd {
        for names_only in [false, true] {
            let r = t.index.search(&SearchRequest { query: q.into(), names_only, ..Default::default() });
            assert!(r.is_ok(), "{q:?}: {r:?}");
        }
    }
    assert_eq!(names(&t.index, "LIGHTHOUSE"), v(&["notes.txt"]));
    assert_eq!(names(&t.index, "Lighthouse Budget"), v(&["notes.txt"]));
    assert_eq!(names(&t.index, "“lighthouse project”"), v(&["notes.txt"]));
    assert_eq!(names(&t.index, "ext:md ext:txt").len(), 7, "{:?}", names(&t.index, "ext:md ext:txt"));
    assert_eq!(names(&t.index, "kind:pdf kind:image"), v(&["🎉 party 🎈.jpg"]));
    assert_eq!(names(&t.index, "类型：文档").len(), names(&t.index, "kind:document").len());
    assert_eq!(names(&t.index, "name:\"🎉 party\""), v(&["🎉 party 🎈.jpg"]));
    // Paging past the end is empty, not an error.
    let r = t.index.search(&SearchRequest { query: "kind:document".into(), offset: 1_000_000, ..Default::default() }).unwrap();
    assert!(r.hits.is_empty());
    assert!(r.total > 0);
}

#[test]
fn folders_moving_in_out_and_into_skipped_places() {
    let t = tree();
    let (index, home) = (&t.index, &t.home);
    let outside = home.parent().unwrap().join("outside");
    put(&outside.join("Stash/old plan.txt"), b"the albatross plan");
    set_mtime(&outside.join("Stash/old plan.txt"), 90 * 86_400);
    scan(index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    index_content(index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let watcher = Watcher::start(index, &t.cfg, |_| {}).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    fs::rename(outside.join("Stash"), home.join("Stash")).unwrap(); // in from outside
    fs::rename(home.join("Deep"), outside.join("Deep")).unwrap(); // out of the roots
    fs::rename(home.join("Movies/clip.MOV"), home.join("Movies/.clip.MOV")).unwrap(); // hidden now
    fs::rename(home.join("Documents/contract.docx"), home.join("Documents/contract.csv")).unwrap(); // *.csv
    fs::rename(home.join("Music"), home.join("build")).unwrap_or_else(|_| {
        fs::rename(home.join("Music/song.mp3"), home.join("build/song.mp3")).unwrap();
    }); // into an excluded folder name
    fs::remove_dir_all(home.join("Pictures")).unwrap();

    wait_until("the folder from outside", || names(index, "albatross") == v(&["old plan.txt"]));
    wait_until("the folder that left", || names(index, "bottom").is_empty());
    wait_until("the hidden file", || names(index, "clip").is_empty());
    wait_until("the skipped ext", || names(index, "contract").is_empty());
    wait_until("the excluded folder", || names(index, "song").is_empty());
    wait_until("the deleted folder", || names(index, "party").is_empty());
    drop(watcher);
    let r = scan(index, &t.cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(r.added + r.removed, 0, "live updates and a rescan disagree: {r:?}");
}

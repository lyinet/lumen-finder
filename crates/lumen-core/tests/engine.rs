use lumen_core::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn touch(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn setup() -> (tempfile::TempDir, Index, Config) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("Documents/工作/2024年终总结.key"), b"keynote");
    touch(&home.join("Documents/工作/劳动合同.pdf"), b"pdf-a");
    touch(&home.join("Downloads/invoice-march.pdf"), b"pdf-b");
    touch(&home.join("Music/Album/01 Intro.flac"), b"flac");
    touch(&home.join("Movies/trip.mov"), &vec![7u8; 4096]);
    touch(&home.join("Desktop/old/trip copy.mov"), &vec![7u8; 4096]);
    // Things that must be ignored
    touch(&home.join("Library/Caches/thing.pdf"), b"x");
    touch(&home.join("code/app/node_modules/pkg/readme.md"), b"x");
    touch(&home.join(".config/notes.md"), b"x");
    touch(&home.join("Pictures/Photos Library.photoslibrary/originals/a.jpg"), b"x");
    touch(&home.join("Documents/lib.dylib"), b"x");
    touch(&home.join("Documents/~$draft.docx"), b"x");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home], excluded_paths: vec![dir.path().join("home/Library")], ..Config::default() };
    (dir, index, cfg)
}

fn q(index: &Index, query: &str) -> Vec<String> {
    let r = index.search(&SearchRequest { query: query.into(), ..Default::default() }).unwrap();
    r.hits.into_iter().map(|h| h.name).collect()
}

#[test]
fn full_scan_search_and_incremental() {
    let (dir, index, cfg) = setup();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut ticks = 0;
    let r = scan(&index, &cfg, cancel.clone(), |_| ticks += 1).unwrap();
    assert!(r.full);
    assert_eq!(r.files, 6, "only personal files are indexed");
    assert_eq!(r.added, 6);
    assert!(ticks > 0);

    // Text, Chinese substrings (2 chars → LIKE path), pinyin, initials, filters.
    assert_eq!(q(&index, "合同"), vec!["劳动合同.pdf"]);
    assert_eq!(q(&index, "年终总结"), vec!["2024年终总结.key"]);
    assert_eq!(q(&index, "hetong"), vec!["劳动合同.pdf"]);
    assert_eq!(q(&index, "nzzj"), vec!["2024年终总结.key"]);
    assert_eq!(q(&index, "invoice"), vec!["invoice-march.pdf"]);
    assert_eq!(q(&index, "kind:pdf").len(), 2);
    assert_eq!(q(&index, "类型:音乐"), vec!["01 Intro.flac"]);
    assert_eq!(q(&index, "trip in:Desktop"), vec!["trip copy.mov"]);
    assert_eq!(q(&index, "pdf in:工作"), vec!["劳动合同.pdf"]); // folder name hit via FTS? no: ext-less term matches parent
    assert_eq!(q(&index, ">1kb").len(), 2);

    // Relevance: exact stem first
    let names = q(&index, "trip");
    assert_eq!(names[0], "trip.mov");

    // Facets ignore the kind filter
    let r = index.search(&SearchRequest { query: "pdf".into(), kinds: vec![Kind::Video], ..Default::default() }).unwrap();
    assert!(r.kind_counts.get("pdf").copied().unwrap_or(0) >= 1);

    // Duplicates
    let d = index.duplicates(1024, 10).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].files.len(), 2);

    // Incremental: add, change, delete
    let home = dir.path().join("home");
    touch(&home.join("Documents/new idea.md"), b"hello");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    touch(&home.join("Downloads/invoice-march.pdf"), b"pdf-b-v2-longer");
    fs::remove_file(home.join("Music/Album/01 Intro.flac")).unwrap();
    let r = scan(&index, &cfg, cancel, |_| {}).unwrap();
    assert!(!r.full);
    assert_eq!((r.added, r.changed, r.removed), (1, 1, 1));
    assert_eq!(r.files, 6);
    assert!(q(&index, "Intro").is_empty());

    let ov = index.overview().unwrap();
    assert!(ov.kinds.iter().any(|k| k.kind == Kind::Pdf && k.files == 2));
}

#[test]
fn watcher_apply_paths() {
    let (dir, index, cfg) = setup();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let _w = Watcher::start(&index, &cfg, move |b| { let _ = tx.send(b); }).unwrap();
    let home = dir.path().join("home");
    std::thread::sleep(std::time::Duration::from_millis(300));
    touch(&home.join("Documents/fresh/报价单.xlsx"), b"xlsx");
    let batch = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("watcher event");
    assert!(batch.upserted >= 1);
    assert_eq!(q(&index, "报价"), vec!["报价单.xlsx"]);
    fs::remove_dir_all(home.join("Documents/fresh")).unwrap();
    let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
    assert!(q(&index, "报价").is_empty());
}

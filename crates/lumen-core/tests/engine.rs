use lumen_core::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// The OCR helper is process-wide; tests that read content take turns.
static CONTENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn docx(p: &Path, text: &str) {
    use std::io::Write;
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    z.start_file("[Content_Types].xml", o).unwrap();
    z.write_all(b"<Types/>").unwrap();
    z.start_file("word/document.xml", o).unwrap();
    write!(z, "<w:document><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>").unwrap();
    z.finish().unwrap();
}

/// A one-page PDF with a real text layer, built by hand.
fn pdf(p: &Path, text: &str) {
    let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1));
    for o in offs {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1));
    touch(p, out.as_bytes());
}

#[test]
fn content_search() {
    let _turn = CONTENT.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    touch(&home.join("Notes/随手记.md"), "# 周末\n去京都看红叶，顺便买抹茶。".as_bytes());
    docx(&home.join("Work/agreement.docx"), "本合同期限为三年，自 2024 年 1 月 1 日起生效。");
    let (gbk, _, _) = encoding_rs::GB18030.encode("老电脑里的会议纪要：预算批准");
    touch(&home.join("Old/minutes.txt"), &gbk);
    pdf(&home.join("Finance/report.pdf"), "Quarterly revenue grew forty percent");
    touch(&home.join("Movies/clip.mov"), b"not text");
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();

    let mut last = None;
    let r = index_content(&index, Arc::new(AtomicBool::new(false)), |p| last = Some(p.clone())).unwrap();
    assert_eq!(r.read, 4, "md, docx, txt, pdf");
    assert_eq!(r.with_text, 4);
    assert!(last.unwrap().finished);
    assert_eq!(index.status().unwrap().content_files, 4);

    // ≥3 chars → content FTS; 2 CJK chars → substring scan.
    assert_eq!(q(&index, "看红叶"), vec!["随手记.md"]);
    assert_eq!(q(&index, "抹茶"), vec!["随手记.md"]);
    assert_eq!(q(&index, "期限为三年"), vec!["agreement.docx"]);
    assert_eq!(q(&index, "会议纪要"), vec!["minutes.txt"]);
    assert_eq!(q(&index, "revenue grew"), vec!["report.pdf"]);
    assert_eq!(q(&index, "revenue kind:pdf"), vec!["report.pdf"]);
    assert!(q(&index, "name:revenue").is_empty(), "name: skips content");

    let res = index.search(&SearchRequest { query: "期限".into(), ..Default::default() }).unwrap();
    let snip = res.hits[0].snippet.clone().unwrap();
    assert!(snip.contains("期限"), "{snip}");
    let res = index.search(&SearchRequest { query: "期限".into(), names_only: true, ..Default::default() }).unwrap();
    assert_eq!(res.total, 0);

    // Second pass reads nothing; an edited file is read again.
    assert_eq!(index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap().read, 0);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    touch(&home.join("Notes/随手记.md"), "改成去北海道滑雪".as_bytes());
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert_eq!(index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap().read, 1);
    assert!(q(&index, "看红叶").is_empty());
    assert_eq!(q(&index, "北海道"), vec!["随手记.md"]);

    // Deleting the file drops its text too.
    fs::remove_file(home.join("Work/agreement.docx")).unwrap();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    assert!(q(&index, "期限为三年").is_empty());
    assert_eq!(index.status().unwrap().content_files, 3);
}

#[test]
fn volume_paths() {
    use std::path::PathBuf;
    assert_eq!(config::volume_root(Path::new("/Volumes/Passport/Movies/a.mp4")), Some(PathBuf::from("/Volumes/Passport")));
    assert_eq!(config::volume_root(Path::new("/Users/a/x")), None);
    assert!(config::volume_offline(Path::new("/Volumes/__lumen_not_mounted__/a.mp4")));
    assert!(!config::volume_offline(Path::new("/Users/a/x")));
}

#[cfg(unix)]
#[test]
fn ocr_and_collections() {
    let _turn = CONTENT.lock().unwrap_or_else(|e| e.into_inner());
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let big = |tag: &str| { let mut v = tag.as_bytes().to_vec(); v.resize(30 * 1024, b' '); v };
    touch(&home.join("Pictures/IMG_0412.jpg"), &big("RECEIPT"));
    touch(&home.join("Pictures/IMG_0413.heic"), &big("IDCARD"));
    touch(&home.join("Pictures/beach.jpg"), &big("NOTHING"));
    touch(&home.join("Desktop/Screenshot 2026-11-01 at 10.02.11.png"), &big("NOTHING"));
    touch(&home.join("Pictures/tiny-icon.png"), b"RECEIPT");
    docx(&home.join("Work/服务协议.docx"), "甲方与乙方经友好协商。");
    docx(&home.join("Work/notes.docx"), "本合同自签字之日起生效。");
    docx(&home.join("Work/plain.docx"), "nothing to see");
    // A stand-in for lumen-ocr: the "image" starts with a tag naming its text.
    let helper = dir.path().join("fake-ocr");
    touch(&helper, b"#!/bin/sh\ncase \"$(head -c 7 \"$1\")\" in\n  RECEIPT) echo '\xe5\x8f\x91\xe7\xa5\xa8\xe4\xbb\xa3\xe7\xa0\x81 044031900111 \xe4\xbb\xb7\xe7\xa8\x8e\xe5\x90\x88\xe8\xae\xa1 \xc2\xa5128.00';;\n  IDCARD*) echo '\xe5\xb1\x85\xe6\xb0\x91\xe8\xba\xab\xe4\xbb\xbd\xe8\xaf\x81 \xe5\x85\xac\xe6\xb0\x91\xe8\xba\xab\xe4\xbb\xbd\xe5\x8f\xb7\xe7\xa0\x81';;\n  *) ;;\nesac\n");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();

    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    set_ocr_helper(Some(helper));
    let r = index_content(&index, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    set_ocr_helper(None);
    assert_eq!(r.read, 7, "3 docx + 4 images over the size floor");

    assert_eq!(q(&index, "价税合计"), vec!["IMG_0412.jpg"]);
    assert_eq!(q(&index, "居民身份证"), vec!["IMG_0413.heic"]);

    let names = |id: &str| {
        let mut v: Vec<String> = index.collection(id, 50, 0).unwrap().into_iter().map(|f| f.name).collect();
        v.sort();
        v
    };
    assert_eq!(names("invoices"), vec!["IMG_0412.jpg"]);
    assert_eq!(names("ids"), vec!["IMG_0413.heic"]);
    assert_eq!(names("contracts"), vec!["notes.docx", "服务协议.docx"]);
    assert_eq!(names("screenshots"), vec!["Screenshot 2026-11-01 at 10.02.11.png"]);
    let stats = index.collections().unwrap();
    assert_eq!(stats.iter().find(|s| s.id == "contracts").unwrap().files, 2);
    assert_eq!(stats.iter().find(|s| s.id == "resumes").unwrap().files, 0);
}

#[test]
fn timeline_by_month() {
    let (dir, index, cfg) = setup();
    let old = dir.path().join("home/Documents/工作/劳动合同.pdf");
    // 2023-06-15 12:00 UTC: mid-month, so any local time zone lands in June.
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_686_830_400);
    fs::File::options().write(true).open(&old).unwrap().set_modified(t).unwrap();
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();

    let months = index.timeline(24).unwrap();
    assert_eq!(months.iter().map(|m| m.files).sum::<i64>(), 6);
    let june = months.iter().find(|m| m.month == "2023-06").expect("June 2023 is on the timeline");
    assert_eq!(june.files, 1);
    assert_eq!(june.kinds[0].kind, Kind::Pdf);
    assert_eq!(months.last().unwrap().month, "2023-06", "newest month first");

    let files = index.month("2023-06", None, 50, 0).unwrap();
    assert_eq!(files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["劳动合同.pdf"]);
    assert!(index.month("2023-06", Some(Kind::Image), 50, 0).unwrap().is_empty());
    assert!(index.month("2023-06'--", None, 50, 0).unwrap().is_empty());
}

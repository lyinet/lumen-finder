//! Load test: a big synthetic index must stay fast (v0.8).
//!
//! Ignored by default (it builds a few hundred thousand rows). Run with:
//!   cargo test -p lumen-core --release --test scale -- --ignored --nocapture
//! LUMEN_SCALE_FILES sets the size (default 300 000).
//!
//! The data is deliberately unkind: names are built from 25 words, so a
//! common word like 合同 matches ~8% of all files (a real Mac matches far
//! fewer). Budgets are for the Linux CI/sandbox box, which runs this 2–4×
//! slower than Apple silicon; on a Mac every search should land under 50 ms.

use lumen_core::index::NewFile;
use lumen_core::*;
use std::time::{Duration, Instant};

const WORDS: &[&str] = &[
    "合同", "发票", "年终总结", "简历", "报价单", "会议纪要", "旅行", "预算", "截屏", "论文", "invoice", "report", "draft",
    "final", "meeting", "photo", "IMG", "Screenshot", "budget", "trip", "北海道", "家庭", "项目", "设计稿", "素材",
];
const EXTS: &[(&str, Kind)] = &[
    ("pdf", Kind::Pdf), ("docx", Kind::Document), ("xlsx", Kind::Spreadsheet), ("key", Kind::Presentation),
    ("jpg", Kind::Image), ("heic", Kind::Image), ("mp4", Kind::Video), ("mov", Kind::Video), ("mp3", Kind::Audio),
    ("zip", Kind::Archive), ("md", Kind::Document), ("png", Kind::Image),
];

fn synth(n: usize) -> Vec<NewFile> {
    let mut seed: u64 = 42;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let now = 1_790_000_000i64;
    (0..n)
        .map(|i| {
            let a = WORDS[(rnd() % WORDS.len() as u64) as usize];
            let b = WORDS[(rnd() % WORDS.len() as u64) as usize];
            let (ext, kind) = EXTS[(rnd() % EXTS.len() as u64) as usize];
            let name = format!("{a}-{b} {i}.{ext}");
            let parent = format!("/Users/lin/{}/{}/{}", ["Documents", "Desktop", "Downloads", "Movies", "Pictures"][i % 5], a, i % 997);
            let mtime = now - (rnd() % (8 * 365 * 86_400)) as i64;
            NewFile {
                path: format!("{parent}/{name}"),
                name_py: pinyin_key(&name),
                name,
                parent,
                ext: ext.into(),
                kind,
                size: (rnd() % 400_000_000) as i64,
                mtime,
                ctime: mtime,
                cloud: false,
            }
        })
        .collect()
}

fn time<T>(label: &str, budget_ms: u64, f: impl Fn() -> T) -> Duration {
    let t = Instant::now();
    f(); // the first call also warms the page cache
    let first = t.elapsed();
    let runs = 5;
    let t = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(f());
    }
    let avg = t.elapsed() / runs;
    println!(
        "{label:<34} {:>8.1} ms  first {:>8.1} ms   (budget {budget_ms} ms)",
        avg.as_secs_f64() * 1000.0,
        first.as_secs_f64() * 1000.0
    );
    // Summaries are memoised until the index changes; their cost is the first call.
    if label.starts_with('~') { first } else { avg }
}

#[test]
#[ignore]
fn large_index_stays_fast() {
    let n: usize = std::env::var("LUMEN_SCALE_FILES").ok().and_then(|v| v.parse().ok()).unwrap_or(300_000);
    let dir = tempfile::tempdir().unwrap();
    let index = Index::open(dir.path().join("index.db")).unwrap();
    let t = Instant::now();
    for chunk in synth(n).chunks(20_000) {
        index.insert_files(chunk).unwrap();
    }
    println!("\n{n} files inserted in {:.1} s", t.elapsed().as_secs_f64());
    let s = |q: &str| {
        let q = q.to_string();
        let index = &index;
        move || index.search(&SearchRequest { query: q.clone(), ..Default::default() }).unwrap().total
    };
    let mut slow = Vec::new();
    let mut check = |label: &str, budget: u64, d: Duration| {
        if d > Duration::from_millis(budget) {
            slow.push(format!("{label}: {:.0} ms > {budget} ms", d.as_secs_f64() * 1000.0));
        }
    };
    check("search 合同 (2 chars)", 120, time("search 合同 (2 chars)", 120, s("合同")));
    check("search 年终总结", 120, time("search 年终总结", 120, s("年终总结")));
    check("search nzzj (pinyin)", 120, time("search nzzj (pinyin)", 120, s("nzzj")));
    check("search invoice kind:pdf", 120, time("search invoice kind:pdf", 120, s("invoice kind:pdf")));
    check("search 合同 >10mb after:2024", 120, time("search 合同 >10mb after:2024", 120, s("合同 >10mb after:2024")));
    check("search (empty, newest)", 120, time("search (empty, newest)", 120, s("")));
    check("search zzz (no hits)", 120, time("search zzz (no hits)", 120, s("zzzq")));
    check("~overview", 2500, time("~overview", 2500, || index.overview().unwrap()));
    check("~timeline", 1200, time("~timeline", 1200, || index.timeline(240).unwrap()));
    check("~collections", 2000, time("~collections", 2000, || index.collections().unwrap()));
    check("forgotten", 200, time("forgotten", 200, || index.forgotten(40).unwrap()));
    check("largest", 50, time("largest", 50, || index.largest(40).unwrap()));
    check("recent", 50, time("recent", 50, || index.recent(40).unwrap()));
    assert!(slow.is_empty(), "over budget at {n} files:\n{}", slow.join("\n"));
}

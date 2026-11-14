//! # lumen-core
//!
//! The engine behind Lumen: find every personal file on a Mac once, keep the
//! index fresh incrementally, and answer "where did I put it?" in
//! milliseconds. UI-free and platform-neutral so it can be tested anywhere
//! and reused by the app, the CLI and future surfaces (menu bar, Raycast,
//! Shortcuts).

pub mod config;
pub mod index;
pub mod insights;
pub mod kind;
pub mod scanner;
pub mod search;
pub mod watcher;

pub use config::Config;
pub use index::{FileRecord, Index, IndexStatus};
pub use insights::{DuplicateGroup, KindStat, Overview};
pub use kind::Kind;
pub use scanner::{scan, ScanPhase, ScanProgress, ScanReport};
pub use search::{SearchRequest, SearchResponse, Sort};
pub use watcher::{ChangeBatch, Watcher};

/// Pinyin search key for a file name: full syllables then initials, so
/// "年终总结.key" is found by "nianzhong" and by "nzzj". Empty for names
/// without Chinese characters (nothing to add over the name itself).
pub fn pinyin_key(name: &str) -> String {
    use pinyin::ToPinyin;
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if !stem.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return String::new();
    }
    let mut full = String::new();
    let mut initials = String::new();
    for (c, py) in stem.chars().zip(stem.to_pinyin()) {
        match py {
            Some(p) => {
                full.push_str(p.plain());
                initials.push_str(p.first_letter());
            }
            None if c.is_alphanumeric() => {
                let l = c.to_lowercase().to_string();
                full.push_str(&l);
                initials.push_str(&l);
            }
            None => {}
        }
    }
    format!("{full} {initials}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinyin() {
        assert_eq!(super::pinyin_key("年终总结.key"), "nianzhongzongjie nzzj");
        assert_eq!(super::pinyin_key("report.pdf"), "");
        assert_eq!(super::pinyin_key("2024合同.pdf"), "2024hetong 2024ht");
    }
}

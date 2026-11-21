//! # lumen-core
//!
//! The engine behind Lumen: find every personal file on a Mac once, keep the
//! index fresh incrementally, and answer "where did I put it?" in
//! milliseconds. UI-free and platform-neutral so it can be tested anywhere
//! and reused by the app, the CLI and future surfaces (menu bar, Raycast,
//! Shortcuts).

pub mod config;
pub mod cjk;
pub mod collections;
pub mod content;
pub mod index;
pub mod insights;
pub mod kind;
pub mod pace;
pub mod scanner;
pub mod search;
pub mod semantic;
pub mod timeline;
pub mod watcher;

pub use config::{Config, Provider};
pub use collections::CollectionStat;
pub use content::{index_content, set_ocr_helper, ContentProgress, ContentReport};
pub use index::{FileRecord, Index, IndexStatus, Suggestion};
pub use insights::{DuplicateGroup, KindStat, Overview};
pub use kind::Kind;
pub use scanner::{scan, ScanPhase, ScanProgress, ScanReport};
pub use search::{SearchRequest, SearchResponse, Sort};
pub use semantic::{
    index_semantic, semantic_search, set_embed_helper, set_embedder, Embedder, SemanticHit, SemanticProgress,
    SemanticReport, SemanticRequest, SemanticResponse,
};
pub use timeline::{KindCount, MonthStat};
pub use watcher::{ChangeBatch, Watcher};

/// Pinyin search key for a file name: full syllables then initials, so
/// "年终总结.key" is found by "nianzhong" and by "nzzj". Empty for names
/// without Chinese characters (nothing to add over the name itself).
/// Text in its precomposed Unicode form (NFC), as people type it. macOS
/// spells some names decomposed (NFD: "e" + accent, Hangul as letters), and
/// a typed query never matched those. Names, folders, document text and
/// queries all go through this; paths keep the disk's own spelling.
pub fn nfc(s: &str) -> std::borrow::Cow<'_, str> {
    use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};
    if s.is_ascii() || is_nfc_quick(s.chars()) == IsNormalized::Yes {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(s.nfc().collect())
    }
}

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

/// Latin letters without their accents, lower-cased, so "resume" finds
/// "Résumé.png" and "cafe" finds "Café" (v0.9). Only Latin letters are
/// touched: kana, Hangul and Chinese keep every mark (が is not か), and so
/// does every other script. Ligatures and the few letters with no
/// decomposition (ß, æ, ø, œ, ł, đ, þ) are spelled out.
pub fn fold(s: &str) -> String {
    use unicode_normalization::char::{decompose_canonical, is_combining_mark};
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        let latin = matches!(c as u32, 0x00C0..=0x024F | 0x1E00..=0x1EFF);
        // A combining accent typed or stored decomposed ("e" + U+0301).
        if (0x0300..=0x036F).contains(&(c as u32)) {
            continue;
        }
        if !latin {
            out.extend(c.to_lowercase());
            continue;
        }
        let spelled = match c {
            'ß' | 'ẞ' => Some("ss"),
            'æ' | 'Æ' => Some("ae"),
            'œ' | 'Œ' => Some("oe"),
            'ø' | 'Ø' => Some("o"),
            'ł' | 'Ł' => Some("l"),
            'đ' | 'Đ' | 'ð' | 'Ð' => Some("d"),
            'þ' | 'Þ' => Some("th"),
            'ı' => Some("i"),
            'ħ' | 'Ħ' => Some("h"),
            _ => None,
        };
        if let Some(s) = spelled {
            out.push_str(s);
            continue;
        }
        decompose_canonical(c, |d| {
            if !is_combining_mark(d) {
                out.extend(d.to_lowercase());
            }
        });
    }
    out
}

/// What `files.name_py` holds for a name: its pinyin (see `pinyin_key`) and,
/// for a name with accents, the name folded (see `fold`) in front, ended by
/// a tab so ranking can tell the folded name from the pinyin:
/// "Résumé.png" → "resume.png\t", "Café合同.pdf" → "cafe合同.pdf\tcafehetong cafeht".
/// Plain names are unchanged from before 0.9 (pinyin only, or empty).
pub fn name_key(name: &str) -> String {
    let py = pinyin_key(name);
    let folded = fold(name);
    if folded == name.to_lowercase() {
        return py;
    }
    format!("{folded}\t{}", fold(&py))
}

/// Accents changed by `fold` (true when the folded word differs).
pub fn has_accents(s: &str) -> bool {
    fold(s) != s.to_lowercase()
}

#[cfg(test)]
mod tests {
    #[test]
    fn folds_latin_only() {
        assert_eq!(super::fold("Résumé.PNG"), "resume.png");
        assert_eq!(super::fold("Cafe\u{301}"), "cafe");
        assert_eq!(super::fold("Straße Øl Æsir"), "strasse ol aesir");
        assert_eq!(super::fold("Tiếng Việt"), "tieng viet");
        // Japanese voiced kana and Hangul keep their marks; Chinese is untouched.
        assert_eq!(super::fold("がぎ한국合同"), "がぎ한국合同");
        assert_eq!(super::name_key("report.pdf"), "");
        assert_eq!(super::name_key("年终总结.key"), "nianzhongzongjie nzzj");
        assert_eq!(super::name_key("Résumé.png"), "resume.png\t");
        assert_eq!(super::name_key("Café合同.pdf"), "cafe合同.pdf\tcafehetong cafeht");
        assert!(super::has_accents("Café") && !super::has_accents("cafe") && !super::has_accents("合同"));
    }

    #[test]
    fn pinyin() {
        assert_eq!(super::pinyin_key("年终总结.key"), "nianzhongzongjie nzzj");
        assert_eq!(super::pinyin_key("report.pdf"), "");
        assert_eq!(super::pinyin_key("2024合同.pdf"), "2024hetong 2024ht");
    }
}

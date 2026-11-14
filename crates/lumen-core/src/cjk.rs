//! Word splitting for the document-text index.
//!
//! SQLite's `unicode61` tokenizer splits on spaces and punctuation, which
//! works for English but sees a whole Chinese sentence as one "word". Before
//! text goes into the index, every run of CJK characters is rewritten as its
//! overlapping two-character pieces (北海道雪景 → 北海 海道 道雪 雪景), and a
//! query is rewritten the same way into a phrase, so 海道雪 matches as the
//! adjacent pieces 海道 道雪. Two characters, the most common length of a
//! Chinese word, become a single token lookup instead of a scan, and the index
//! is far smaller than a trigram index over every character.

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF      // Hiragana, Katakana
        | 0x3400..=0x4DBF    // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0x20000..=0x2FA1F) // Extensions B–F, compatibility supplement
}

/// One piece of a split string.
#[derive(Debug, PartialEq)]
enum Piece {
    Cjk(String),
    Other(String),
}

fn pieces(s: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut run: Vec<char> = Vec::new();
    let mut other = String::new();
    let flush_run = |run: &mut Vec<char>, out: &mut Vec<Piece>| {
        match run.len() {
            0 => {}
            1 => out.push(Piece::Cjk(run[0].to_string())),
            _ => {
                for w in run.windows(2) {
                    out.push(Piece::Cjk(w.iter().collect()));
                }
            }
        }
        run.clear();
    };
    for c in s.chars() {
        if is_cjk(c) {
            if !other.is_empty() {
                out.push(Piece::Other(std::mem::take(&mut other)));
            }
            run.push(c);
        } else {
            flush_run(&mut run, &mut out);
            other.push(c);
        }
    }
    flush_run(&mut run, &mut out);
    if !other.is_empty() {
        out.push(Piece::Other(other));
    }
    out
}

/// The text as it goes into the index.
pub fn index_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 2);
    for p in pieces(s) {
        if !out.is_empty() {
            out.push(' ');
        }
        match p {
            Piece::Cjk(t) | Piece::Other(t) => out.push_str(&t),
        }
    }
    out
}

/// Plain words of a non-CJK piece, the way unicode61 would see them.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// An FTS5 MATCH expression for one search word, or None when the word is
/// too short to be useful against document text (a single CJK character, or
/// one or two Latin letters, would match nearly every document).
pub fn match_expr(term: &str) -> Option<String> {
    let cjk_chars = term.chars().filter(|c| is_cjk(*c)).count();
    let other_chars = term.chars().filter(|c| c.is_alphanumeric() && !is_cjk(*c)).count();
    if cjk_chars < 2 && cjk_chars + other_chars < 3 {
        return None;
    }
    let mut toks: Vec<(String, bool)> = Vec::new(); // (token, is_cjk_single)
    for p in pieces(term) {
        match p {
            Piece::Cjk(t) => {
                let single = t.chars().count() == 1;
                toks.push((t, single));
            }
            Piece::Other(t) => toks.extend(words(&t).into_iter().map(|w| (w, false))),
        }
    }
    let (last, last_single) = toks.last()?.clone();
    // A trailing Latin word, or a lone trailing CJK character, may be the
    // start of something longer in the text (report → reports, 报 → 报告).
    let prefix = last_single || !last.chars().any(is_cjk);
    let phrase = toks.iter().map(|(t, _)| t.replace('"', "")).collect::<Vec<_>>().join(" ");
    Some(if prefix { format!("\"{phrase}\" *") } else { format!("\"{phrase}\"") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text() {
        assert_eq!(index_text("北海道雪景"), "北海 海道 道雪 雪景");
        assert_eq!(index_text("Q3报告 final"), "Q3 报告  final");
        assert_eq!(index_text("合"), "合");
    }

    #[test]
    fn queries() {
        assert_eq!(match_expr("合同").as_deref(), Some("\"合同\""));
        assert_eq!(match_expr("违约金").as_deref(), Some("\"违约 约金\""));
        assert_eq!(match_expr("Revenue").as_deref(), Some("\"revenue\" *"));
        assert_eq!(match_expr("Q3报").as_deref(), Some("\"q3 报\" *"));
        assert_eq!(match_expr("合"), None);
        assert_eq!(match_expr("ab"), None);
    }
}

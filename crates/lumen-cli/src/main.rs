//! `lumen` — the engine from a terminal. Shares the app's index by default.
//!
//!   lumen scan [ROOT…]          full or incremental scan
//!   lumen search QUERY…         search (supports kind:, ext:, in:, >10mb, after:…)
//!   lumen content               read the words inside documents (正文搜索)
//!   lumen ask QUERY…            semantic search: find files by what they are about
//!                               (macOS; uses the helper inside Lumen.app)
//!   lumen stats                 totals by kind, scattered folders
//!   lumen dupes                 likely duplicate files
//!   lumen --db PATH …           use another index file
//!   lumen --json …              machine-readable output
//!   lumen --alfred search|ask … Alfred Script Filter JSON (see integrations/alfred)
//!
//! Lumen.app ships this binary at Lumen.app/Contents/MacOS/lumen; the Homebrew
//! cask links it onto PATH.

use anyhow::Result;
use lumen_core::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let alfred = take_flag(&mut args, "--alfred");
    let json = take_flag(&mut args, "--json") && !alfred;
    let db = take_opt(&mut args, "--db")?.map(PathBuf::from).unwrap_or_else(default_db);
    let cmd = if args.is_empty() { "help".to_string() } else { args.remove(0) };
    let index = Index::open(&db)?;
    set_embed_helper(Some(helper_path()));

    match cmd.as_str() {
        "scan" => {
            let mut cfg = Config::load(&config_path());
            if !args.is_empty() {
                cfg.roots = args.iter().map(PathBuf::from).collect();
            }
            let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |p| {
                if !json {
                    eprint!("\r\x1b[2K  {:>9} files · {:>7} folders · {}", p.files_found, p.dirs, human(p.bytes_found as i64));
                }
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                eprintln!();
                println!(
                    "{} {} files ({}) in {:.2}s · +{} added · {} changed · −{} removed",
                    if r.full { "Indexed" } else { "Updated" },
                    r.files,
                    human(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                    r.added,
                    r.changed,
                    r.removed
                );
            }
            if cfg.content_search && !r.cancelled {
                read_content(&index, json)?;
            }
        }
        "content" => read_content(&index, json)?,
        "search" | "s" => {
            let r = index.search(&SearchRequest { query: args.join(" "), limit: 30, ..Default::default() })?;
            if alfred {
                print_alfred(r.hits.iter().map(|h| (h, h.snippet.as_deref())))?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for h in &r.hits {
                    let tag = if h.offline {
                        "  (offline)"
                    } else if h.cloud {
                        "  (iCloud)"
                    } else {
                        ""
                    };
                    println!("{:<12} {:>9}  {}{tag}", h.kind.slug(), human(h.size), h.path);
                    if let Some(s) = &h.snippet {
                        println!("{:>24}{s}", "");
                    }
                }
                println!("— {} matches in {:.1} ms", r.total, r.took_us as f64 / 1000.0);
            }
        }
        "ask" => {
            anyhow::ensure!(lumen_core::semantic::available(), "semantic search needs Lumen.app (set LUMEN_HELPER to lumen-ocr)");
            let r = index_semantic(&index, Arc::new(AtomicBool::new(false)), |p| {
                if !json && !alfred && !p.finished && p.total > 0 {
                    eprint!("\r\x1b[2K  learning {}/{} files", p.done, p.total);
                }
            })?;
            if !json && !alfred && r.embedded > 0 {
                eprintln!();
            }
            let r = semantic_search(&index, &SemanticRequest { query: args.join(" "), limit: 20, ..Default::default() })?;
            if alfred {
                print_alfred(r.hits.iter().map(|h| (&h.file, None)))?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for h in &r.hits {
                    println!("{:>4.0}%  {:<12} {}", h.score * 100.0, h.file.kind.slug(), h.file.path);
                }
                println!("— {} matches in {:.1} ms", r.hits.len(), r.took_us as f64 / 1000.0);
            }
        }
        "stats" => {
            let ov = index.overview()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ov)?);
            } else {
                let st = index.status()?;
                println!("{} files · {}", st.files, human(st.bytes));
                for k in &ov.kinds {
                    println!("  {:<13} {:>8} files {:>10}  in {} folders", k.kind.slug(), k.files, human(k.bytes), k.folders);
                }
            }
        }
        "dupes" => {
            let groups = index.duplicates(1024 * 1024, 30)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else {
                for g in &groups {
                    println!("{} × {} (wasting {})", g.files.len(), human(g.size), human(g.wasted));
                    for f in &g.files {
                        println!("    {}", f.path);
                    }
                }
            }
        }
        _ => {
            println!("lumen scan [ROOT…] | content | search QUERY | ask QUERY | stats | dupes   [--db PATH] [--json | --alfred]");
        }
    }
    Ok(())
}

fn read_content(index: &Index, json: bool) -> Result<()> {
    let r = index_content(index, Arc::new(AtomicBool::new(false)), |p| {
        if !json && !p.finished {
            eprint!("\r\x1b[2K  reading {}/{} documents", p.done, p.total);
        }
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        eprintln!();
        println!("Read {} documents ({} with text, {} unreadable) in {:.2}s", r.read, r.with_text, r.failed, r.elapsed_ms as f64 / 1000.0);
    }
    Ok(())
}

/// Alfred Script Filter: one item per file, ↵ opens it, ⌘↵ reveals it
/// (Alfred's own file actions), and Alfred previews it with ⇧ / ⌘Y.
fn print_alfred<'a>(hits: impl Iterator<Item = (&'a FileRecord, Option<&'a str>)>) -> Result<()> {
    let items: Vec<serde_json::Value> = hits
        .map(|(f, snippet)| {
            let place = tilde(&f.parent, &std::env::var("HOME").unwrap_or_default());
            let mut sub = match snippet {
                Some(s) => format!("{s} · {place}"),
                None => place,
            };
            if f.cloud {
                sub = format!("☁ iCloud · {sub}");
            }
            serde_json::json!({
                "uid": f.path,
                "type": "file",
                "title": f.name,
                "subtitle": sub,
                "arg": f.path,
                "quicklookurl": f.path,
                "icon": { "type": "fileicon", "path": f.path },
                "mods": { "cmd": { "arg": f.path, "subtitle": "Reveal in Finder" } },
            })
        })
        .collect();
    let items = if items.is_empty() {
        vec![serde_json::json!({ "title": "No matches", "subtitle": "Try fewer words, or search by meaning with “la”", "valid": false })]
    } else {
        items
    };
    println!("{}", serde_json::json!({ "items": items }));
    Ok(())
}

fn take_flag(args: &mut Vec<String>, f: &str) -> bool {
    let n = args.len();
    args.retain(|a| a != f);
    n != args.len()
}

/// `--db PATH`. A flag with no value is an error: before, `lumen search x --db`
/// quietly used the app's own index instead of the one asked for.
fn take_opt(args: &mut Vec<String>, f: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == f) else { return Ok(None) };
    args.remove(i);
    anyhow::ensure!(i < args.len(), "{f} needs a value");
    Ok(Some(args.remove(i)))
}

/// "/Users/a/Documents" → "~/Documents". Only a whole leading home folder:
/// a plain `replace` turned "/Users/ab" into "~b", and with HOME unset or
/// empty put a "~" between every character.
fn tilde(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("app.lumen.desktop")
}
fn default_db() -> PathBuf {
    data_dir().join("index.db")
}
fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// The embedding helper: $LUMEN_HELPER, else the one next to this binary
/// (inside Lumen.app, also when linked by Homebrew), else /Applications.
fn helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LUMEN_HELPER") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .and_then(|e| e.parent().map(|d| d.join("lumen-ocr")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("/Applications/Lumen.app/Contents/MacOS/lumen-ocr"))
}

fn human(b: i64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", units[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_becomes_tilde_only_as_a_prefix() {
        assert_eq!(tilde("/Users/a/Documents", "/Users/a"), "~/Documents");
        assert_eq!(tilde("/Users/a", "/Users/a/"), "~");
        assert_eq!(tilde("/Users/ab/x", "/Users/a"), "/Users/ab/x");
        assert_eq!(tilde("/Volumes/Users/a/x", "/Users/a"), "/Volumes/Users/a/x");
        assert_eq!(tilde("/Users/a/x", ""), "/Users/a/x");
    }

    #[test]
    fn db_flag_needs_a_value() {
        let mut a: Vec<String> = vec!["search".into(), "x".into(), "--db".into()];
        assert!(take_opt(&mut a, "--db").is_err());
        let mut a: Vec<String> = vec!["--db".into(), "/tmp/i.db".into(), "stats".into()];
        assert_eq!(take_opt(&mut a, "--db").unwrap().as_deref(), Some("/tmp/i.db"));
        assert_eq!(a, vec!["stats".to_string()]);
        let mut a: Vec<String> = vec!["stats".into()];
        assert_eq!(take_opt(&mut a, "--db").unwrap(), None);
    }
}

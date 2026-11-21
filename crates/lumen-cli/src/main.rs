//! `lumen` — the engine from a terminal. Shares the app's index by default.
//!
//!   lumen scan [ROOT…]          full or incremental scan
//!   lumen search QUERY…         search (supports kind:, ext:, in:, cloud:, >10mb, after:…)
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

const USAGE: &str = "lumen scan [ROOT…] | content | search QUERY | ask QUERY | stats | dupes   [--db PATH] [--json | --alfred]";

fn main() -> Result<()> {
    quiet_on_closed_pipe();
    // Not `env::args()`: it panics on an argument that is not UTF-8 (a pasted
    // byte, a name from another system). macOS file names are UTF-8.
    let mut args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let alfred = take_flag(&mut args, "--alfred");
    let json = take_flag(&mut args, "--json") && !alfred;
    let r = run(args, json, alfred);
    match r {
        // Alfred shows a Script Filter's output, never its errors: a failure
        // (no semantic helper, an unreadable index) left an empty list.
        Err(e) if alfred => {
            let item = serde_json::json!({ "title": "Lumen could not search", "subtitle": format!("{e:#}"), "valid": false });
            println!("{}", serde_json::json!({ "items": [item] }));
            Ok(())
        }
        r => r,
    }
}

fn run(mut args: Vec<String>, json: bool, alfred: bool) -> Result<()> {
    let db = take_opt(&mut args, "--db")?.map(PathBuf::from).unwrap_or_else(default_db);
    let cmd = if args.is_empty() { "help".to_string() } else { args.remove(0) };
    if !matches!(cmd.as_str(), "scan" | "content" | "search" | "s" | "ask" | "stats" | "dupes") {
        // Help needs no index (and must not create an empty one).
        if matches!(cmd.as_str(), "help" | "--help" | "-h") {
            println!("{USAGE}");
            return Ok(());
        }
        // A mistyped command is an error: it used to print the usage and
        // exit 0, so a script never noticed nothing ran.
        eprintln!("lumen: unknown command “{cmd}”\n{USAGE}");
        std::process::exit(2);
    }
    let index = Index::open(&db)?;
    set_embed_helper(Some(helper_path()));

    match cmd.as_str() {
        "scan" => {
            let mut cfg = Config::load(&config_path());
            if !args.is_empty() {
                // Full paths, and only folders that exist: "." stored rows
                // like "./notes.txt" the app could not open, and a mistyped
                // folder scanned nothing and then removed every file of the
                // index (it shares the app's) as gone.
                cfg.roots = args
                    .iter()
                    .map(|a| {
                        let p = full_path(a)?;
                        anyhow::ensure!(p.is_dir(), "no such folder: {a}");
                        Ok(p)
                    })
                    .collect::<Result<_>>()?;
            }
            let r = scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |p| {
                if !json {
                    eprint!("\r\x1b[2K  {:>9} files · {:>7} folders · {}", p.files_found, p.dirs, human(p.bytes_found as i64));
                }
            })?;
            if json {
                // One JSON document: the scan report, with the reading pass
                // under "content" when there was one. Two documents in a row
                // were not parseable as JSON.
                let mut out = serde_json::to_value(&r)?;
                if cfg.content_search && !r.cancelled {
                    out["content"] = serde_json::to_value(read_content(&index, true)?)?;
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(());
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
        "content" => {
            let r = read_content(&index, json)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            }
        }
        "search" | "s" => {
            let r = index.search(&SearchRequest { query: args.join(" "), limit: 30, ..Default::default() })?;
            if alfred {
                print_alfred(r.hits.iter().map(|h| (h, h.snippet.as_deref())))?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for h in &r.hits {
                    let tag = if h.offline {
                        "  (offline)".to_string()
                    } else if h.cloud {
                        format!("  ({}, not downloaded)", h.provider.as_ref().map(|p| p.name.as_str()).unwrap_or("iCloud"))
                    } else if let Some(p) = &h.provider {
                        format!("  ({})", p.name)
                    } else {
                        String::new()
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
            let mut shown = false;
            index_semantic(&index, Arc::new(AtomicBool::new(false)), |p| {
                if !json && !alfred && !p.finished && p.total > 0 {
                    eprint!("\r\x1b[2K  learning {}/{} files", p.done, p.total);
                    shown = true;
                }
            })?;
            // End the progress line even when nothing could be embedded.
            if shown {
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
        _ => println!("{USAGE}"),
    }
    Ok(())
}

/// Read documents; prints the summary line unless `json` (the caller prints).
fn read_content(index: &Index, json: bool) -> Result<ContentReport> {
    let r = index_content(index, Arc::new(AtomicBool::new(false)), |p| {
        if !json && !p.finished {
            eprint!("\r\x1b[2K  reading {}/{} documents", p.done, p.total);
        }
    })?;
    if !json {
        eprintln!();
        println!("Read {} documents ({} with text, {} unreadable) in {:.2}s", r.read, r.with_text, r.failed, r.elapsed_ms as f64 / 1000.0);
    }
    Ok(r)
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
                sub = format!("☁ {} · {sub}", f.provider.as_ref().map(|p| p.name.as_str()).unwrap_or("iCloud"));
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

/// `lumen search x | head -1`: once the reader has what it wants it closes
/// the pipe, and the next line printed panicked with "Broken pipe" (Rust
/// ignores SIGPIPE, so a write fails instead). Like every Unix tool, end
/// quietly instead: the default SIGPIPE action, exit status 141 (128 + 13)
/// as the shell reports it, nothing on stderr.
fn quiet_on_closed_pipe() {
    #[cfg(unix)]
    // SAFETY: called first thing in main, before any thread exists;
    // restoring a signal's default action has no other effect.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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

/// An absolute path with "." and ".." worked out, as the app stores paths
/// ("Docs/../Docs" was stored as written: a second copy of every file).
fn full_path(a: &str) -> Result<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in std::path::absolute(a)?.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    Ok(out)
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
    fn roots_are_full_and_tidy() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(full_path(".").unwrap(), cwd);
        assert_eq!(full_path("a/../b/./c/").unwrap(), cwd.join("b/c"));
        assert_eq!(full_path("/x/../y").unwrap(), PathBuf::from("/y"));
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

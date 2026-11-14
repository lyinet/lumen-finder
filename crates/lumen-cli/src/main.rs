//! `lumen` — the engine from a terminal. Shares the app's index by default.
//!
//!   lumen scan [ROOT…]          full or incremental scan
//!   lumen search QUERY…         search (supports kind:, ext:, in:, >10mb, after:…)
//!   lumen stats                 totals by kind, scattered folders
//!   lumen dupes                 likely duplicate files
//!   lumen --db PATH …           use another index file
//!   lumen --json …              machine-readable output

use anyhow::Result;
use lumen_core::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json = take_flag(&mut args, "--json");
    let db = take_opt(&mut args, "--db").map(PathBuf::from).unwrap_or_else(default_db);
    let cmd = if args.is_empty() { "help".to_string() } else { args.remove(0) };
    let index = Index::open(&db)?;

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
        }
        "search" | "s" => {
            let r = index.search(&SearchRequest { query: args.join(" "), limit: 30, ..Default::default() })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for h in &r.hits {
                    println!("{:<12} {:>9}  {}", h.kind.slug(), human(h.size), h.path);
                }
                println!("— {} matches in {:.1} ms", r.total, r.took_us as f64 / 1000.0);
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
            println!("lumen scan [ROOT…] | search QUERY | stats | dupes   [--db PATH] [--json]");
        }
    }
    Ok(())
}

fn take_flag(args: &mut Vec<String>, f: &str) -> bool {
    let n = args.len();
    args.retain(|a| a != f);
    n != args.len()
}

fn take_opt(args: &mut Vec<String>, f: &str) -> Option<String> {
    let i = args.iter().position(|a| a == f)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
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

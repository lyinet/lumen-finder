//! The `lumen` binary's machine-readable output.

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lumen-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    d
}

fn lumen(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lumen")).args(args).env_remove("LUMEN_HELPER").output().unwrap()
}

#[test]
fn scan_json_is_one_document() {
    // With content search on (the default), `scan --json` printed the scan
    // report and then the reading report: two JSON documents, which no
    // parser reads as one.
    let d = scratch("scan");
    std::fs::write(d.join("home/notes.txt"), b"hello world").unwrap();
    let db = d.join("i.db");
    let out = lumen(&["--db", db.to_str().unwrap(), "--json", "scan", d.join("home").to_str().unwrap()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(v["files"], 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn alfred_gets_an_item_not_an_error() {
    // Alfred shows a Script Filter's JSON only; an error left the list blank.
    let d = scratch("alfred");
    let db = d.join("i.db");
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(["--db", db.to_str().unwrap(), "--alfred", "ask", "snow"])
        .env("LUMEN_HELPER", d.join("no-such-helper"))
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("Alfred JSON");
    assert_eq!(v["items"][0]["valid"], false);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn help_does_not_create_an_index() {
    let d = scratch("help");
    let db = d.join("sub/i.db");
    let out = lumen(&["--db", db.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(!db.exists());
    let _ = std::fs::remove_dir_all(&d);
}

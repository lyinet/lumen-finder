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

fn count(db: &std::path::Path) -> i64 {
    let out = lumen(&["--db", db.to_str().unwrap(), "--json", "stats"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["kinds"].as_array().unwrap().iter().map(|k| k["files"].as_i64().unwrap()).sum()
}

#[test]
fn scanning_a_mistyped_folder_does_not_empty_the_index() {
    // `lumen scan ~/Documnets` scanned no roots and then deleted every row
    // not seen, emptying the index the app shares.
    let d = scratch("typo");
    std::fs::write(d.join("home/notes.txt"), b"hello").unwrap();
    let db = d.join("i.db");
    assert!(lumen(&["--db", db.to_str().unwrap(), "scan", d.join("home").to_str().unwrap()]).status.success());
    assert_eq!(count(&db), 1);
    let out = lumen(&["--db", db.to_str().unwrap(), "scan", d.join("hmoe").to_str().unwrap()]);
    assert!(!out.status.success(), "a missing folder is an error");
    assert_eq!(count(&db), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn relative_roots_are_stored_as_full_paths() {
    // `lumen scan .` stored "./notes.txt": the app could not open or reveal
    // it, and a later scan of the full path listed every file twice.
    let d = scratch("rel");
    std::fs::write(d.join("home/notes.txt"), b"hello").unwrap();
    let db = d.join("i.db");
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(["--db", db.to_str().unwrap(), "scan", "."])
        .current_dir(d.join("home"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = lumen(&["--db", db.to_str().unwrap(), "--json", "search", "notes"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = v["hits"][0]["path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&path).is_absolute(), "{path}");
    assert!(std::path::Path::new(&path).exists(), "{path}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unknown_command_fails() {
    // `lumen serch x` printed the usage and exited 0: scripts saw success.
    let d = scratch("unknown");
    let out = lumen(&["--db", d.join("i.db").to_str().unwrap(), "serch", "x"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("lumen scan"));
    assert!(lumen(&["help"]).status.success());
    assert!(lumen(&["--help"]).status.success());
    assert!(lumen(&[]).status.success());
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_argument_does_not_panic() {
    use std::os::unix::ffi::OsStrExt;
    let d = scratch("bytes");
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .arg("--db")
        .arg(d.join("i.db"))
        .arg("search")
        .arg(std::ffi::OsStr::from_bytes(b"caf\xff"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(unix)]
#[test]
fn a_closed_pipe_ends_quietly() {
    // `lumen search x | head -1`: the reader goes away and the next line
    // printed used to panic with "failed printing to stdout: Broken pipe".
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    let d = scratch("pipe");
    for i in 0..40 {
        std::fs::write(d.join(format!("home/report-{i}.txt")), b"x").unwrap();
    }
    let db = d.join("i.db");
    assert!(lumen(&["--db", db.to_str().unwrap(), "scan", d.join("home").to_str().unwrap()]).status.success());
    for _ in 0..3 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumen"))
            .args(["--db", db.to_str().unwrap(), "search", "report"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take()); // the reader is gone before the first line
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked") && !err.contains("Broken pipe"), "{err}");
        // Ended by SIGPIPE (the shell reports 141), or done before it noticed.
        assert!(out.status.success() || out.status.signal() == Some(13), "{:?}", out.status);
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn search_ignores_accents_and_tells_the_cloud_service() {
    let d = scratch("accents");
    std::fs::create_dir_all(d.join("home/Dropbox")).unwrap();
    std::fs::write(d.join("home/R\u{e9}sum\u{e9}.pdf"), b"x").unwrap();
    std::fs::write(d.join("home/Dropbox/plan.pdf"), b"x").unwrap();
    let db = d.join("i.db");
    assert!(lumen(&["--db", db.to_str().unwrap(), "scan", d.join("home").to_str().unwrap()]).status.success());
    let out = lumen(&["--db", db.to_str().unwrap(), "--json", "search", "resume"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hits"][0]["name"], "R\u{e9}sum\u{e9}.pdf");
    // ~/Dropbox is Dropbox's only when it is directly in the home folder.
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(["--db", db.to_str().unwrap(), "search", "cloud:dropbox"])
        .env("HOME", d.join("home"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("plan.pdf") && text.contains("(Dropbox)"), "{text}");
    assert!(!text.contains("sum"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

//! Stopping the watcher must not wait for its own document reading. Its own
//! test binary: the OCR helper is process-wide.
#![cfg(unix)]

use lumen_core::{scan, set_ocr_helper, Config, Index, Watcher};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn stopping_the_watcher_does_not_wait_for_ocr() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("seed.txt"), b"seed text").unwrap();
    // A helper that takes 30 s on every image (a stuck Vision request).
    let mark = dir.path().join("ocr-started");
    let bin = dir.path().join("lumen-ocr");
    std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", mark.display())).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_ocr_helper(Some(bin));

    let index = Index::open(dir.path().join("index.db")).unwrap();
    let cfg = Config { roots: vec![home.clone()], content_search: true, ocr: true, ..Config::default() };
    scan(&index, &cfg, Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let watcher = Watcher::start(&index, &cfg, |_| {}).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(home.join("scan.png"), vec![7u8; 64 * 1024]).unwrap();

    let t = Instant::now();
    while !mark.exists() {
        if t.elapsed() > Duration::from_secs(20) {
            set_ocr_helper(None);
            panic!("the watcher never read the new image");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let t = Instant::now();
    drop(watcher);
    let took = t.elapsed();
    set_ocr_helper(None);
    assert!(took < Duration::from_secs(3), "stopping the watcher took {took:?}");
}

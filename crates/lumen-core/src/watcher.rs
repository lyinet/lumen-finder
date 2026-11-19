//! Live incremental updates. On macOS `notify` sits on FSEvents, so watching a
//! whole home folder costs one stream, not a handle per directory. Events are
//! coalesced for a short quiet period, then applied in one write.

use crate::config::Config;
use crate::index::Index;
use crate::scanner::apply_paths;
use anyhow::Result;
use notify::{RecursiveMode, Watcher as _};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_millis(700);
const MAX_HOLD: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
pub struct ChangeBatch {
    pub upserted: usize,
    pub removed: usize,
    /// Documents whose words were (re)read after this batch.
    pub content_read: u64,
    /// A drive was plugged in or ejected: the app rescans to pick it up.
    pub volumes_changed: bool,
    /// More documents arrived than one batch reads (a big folder moved in):
    /// the app starts a background reading pass for the rest.
    pub content_left: bool,
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    /// Wakes the thread out of its quiet-period wait when stopping.
    wake: crossbeam_channel::Sender<PathBuf>,
    /// Closed (disconnected) when the thread has finished.
    done: crossbeam_channel::Receiver<()>,
    _inner: notify::RecommendedWatcher,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Which drives coming or going call for a rescan: any drive when external
/// drives are indexed, otherwise the drives a configured root lives on. Only
/// the first case used to watch /Volumes, so a root on a drive that was
/// unplugged at launch stayed offline after the drive came back, until the
/// next launch.
pub(crate) struct DriveWatch {
    all: bool,
    drives: Vec<PathBuf>,
}

impl DriveWatch {
    pub(crate) fn new(cfg: &Config) -> DriveWatch {
        let drives = cfg.roots.iter().filter_map(|r| crate::config::volume_root(r)).collect();
        DriveWatch { all: cfg.include_external, drives }
    }

    fn any(&self) -> bool {
        self.all || !self.drives.is_empty()
    }

    /// `path` is an event's path: a drive mounted or ejected shows up as an
    /// event on /Volumes/<name>.
    pub(crate) fn matters(&self, path: &std::path::Path) -> bool {
        path.parent() == Some(std::path::Path::new(crate::config::VOLUMES)) && (self.all || self.drives.iter().any(|d| d == path))
    }
}

/// How long stopping waits for a batch being written before letting the
/// thread finish on its own (it stops at the next file).
const STOP_WAIT: Duration = Duration::from_secs(1);

impl Watcher {
    pub fn start(
        index: &Index,
        cfg: &Config,
        on_change: impl Fn(ChangeBatch) + Send + 'static,
    ) -> Result<Watcher> {
        let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
        let wake = tx.clone();
        let (done_tx, done) = crossbeam_channel::bounded::<()>(0);
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.kind.is_access() {
                    return;
                }
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        let roots = cfg.effective_roots();
        for r in &roots {
            if let Err(e) = inner.watch(r, RecursiveMode::Recursive) {
                log::warn!("cannot watch {}: {e}", r.display());
            }
        }
        let volumes = PathBuf::from(crate::config::VOLUMES);
        let drives = DriveWatch::new(cfg);
        if drives.any() && volumes.is_dir() {
            let _ = inner.watch(&volumes, RecursiveMode::NonRecursive);
        }
        let content = cfg.content_search;
        let filter = cfg.filter();
        let mut conn = index.writer()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            let _done = done_tx;
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut first: Option<Instant> = None;
            while !stop2.load(Ordering::Relaxed) {
                match rx.recv_timeout(QUIET) {
                    Ok(p) => {
                        if stop2.load(Ordering::Relaxed) {
                            break;
                        }
                        pending.insert(p);
                        first.get_or_insert_with(Instant::now);
                        if first.map(|t| t.elapsed() < MAX_HOLD).unwrap_or(true) {
                            continue;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
                if pending.is_empty() {
                    continue;
                }
                let paths: Vec<PathBuf> = pending.drain().collect();
                first = None;
                let volumes_changed = paths.iter().any(|p| drives.matters(p));
                match apply_paths(&mut conn, &paths, &filter, &roots) {
                    Ok((upserted, removed)) if upserted + removed > 0 || volumes_changed => {
                        let (mut content_read, mut content_left) = (0, false);
                        if content && upserted > 0 {
                            // Stopping the watcher stops its reading too: an
                            // image's OCR can take a minute, and restarting
                            // the watcher used to wait for every one of them.
                            match crate::content::index_pending(&mut conn, &stop2, false, |_| {}) {
                                Ok(r) => (content_read, content_left) = (r.read, r.left),
                                Err(e) => log::warn!("content update failed: {e}"),
                            }
                        }
                        on_change(ChangeBatch { upserted, removed, content_read, volumes_changed, content_left })
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("watcher apply failed: {e}"),
                }
            }
        });
        Ok(Watcher { stop, wake, done, _inner: inner, thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(PathBuf::new());
        if let Some(t) = self.thread.take() {
            // Never block the caller (the app's restart, a settings change)
            // on a document still being parsed: wait briefly, then let the
            // thread end by itself.
            if let Err(crossbeam_channel::RecvTimeoutError::Disconnected) = self.done.recv_timeout(STOP_WAIT) {
                let _ = t.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn drive_with_a_root_is_watched_without_external_drives() {
        let cfg = Config { roots: vec![PathBuf::from("/Users/a"), PathBuf::from("/Volumes/Passport/Movies")], include_external: false, ..Config::default() };
        let d = DriveWatch::new(&cfg);
        assert!(d.any());
        assert!(d.matters(Path::new("/Volumes/Passport")));
        assert!(!d.matters(Path::new("/Volumes/Other")));
        assert!(!d.matters(Path::new("/Volumes/Passport/Movies/a.mp4")));
        let cfg = Config { roots: vec![PathBuf::from("/Users/a")], include_external: false, ..Config::default() };
        assert!(!DriveWatch::new(&cfg).any());
        let cfg = Config { roots: vec![PathBuf::from("/Users/a")], include_external: true, ..Config::default() };
        assert!(DriveWatch::new(&cfg).matters(Path::new("/Volumes/Other")));
    }
}

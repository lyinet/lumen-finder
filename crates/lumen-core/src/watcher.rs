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
}

pub struct Watcher {
    stop: Arc<AtomicBool>,
    _inner: notify::RecommendedWatcher,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(
        index: &Index,
        cfg: &Config,
        on_change: impl Fn(ChangeBatch) + Send + 'static,
    ) -> Result<Watcher> {
        let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
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
        if cfg.include_external && volumes.is_dir() {
            let _ = inner.watch(&volumes, RecursiveMode::NonRecursive);
        }
        let content = cfg.content_search;
        let filter = cfg.filter();
        let mut conn = index.writer()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut first: Option<Instant> = None;
            while !stop2.load(Ordering::Relaxed) {
                match rx.recv_timeout(QUIET) {
                    Ok(p) => {
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
                let volumes_changed = paths.iter().any(|p| p.parent() == Some(volumes.as_path()));
                match apply_paths(&mut conn, &paths, &filter, &roots) {
                    Ok((upserted, removed)) if upserted + removed > 0 || volumes_changed => {
                        let mut content_read = 0;
                        if content && upserted > 0 {
                            let never = Arc::new(AtomicBool::new(false));
                            match crate::content::index_pending(&mut conn, &never, |_| {}) {
                                Ok(r) => content_read = r.read,
                                Err(e) => log::warn!("content update failed: {e}"),
                            }
                        }
                        on_change(ChangeBatch { upserted, removed, content_read, volumes_changed })
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("watcher apply failed: {e}"),
                }
            }
        });
        Ok(Watcher { stop, _inner: inner, thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

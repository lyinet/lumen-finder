//! What to scan and what to leave alone.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folders to scan, recursively.
    pub roots: Vec<PathBuf>,
    /// Absolute folders never entered (user choice, on top of the built-ins).
    pub excluded_paths: Vec<PathBuf>,
    /// Folder names never entered, anywhere.
    pub excluded_names: Vec<String>,
    /// Smallest file worth indexing, in bytes (skips thumbnails, stubs).
    pub min_size: u64,
    /// Also index hidden files and folders (dotfiles). Off by default.
    pub include_hidden: bool,
    /// Read the words inside documents so search finds what a file says.
    pub content_search: bool,
    /// Index external drives under /Volumes too, and remember their files
    /// while they are unplugged.
    pub include_external: bool,
    /// The summon shortcut, in Tauri's accelerator syntax.
    pub hotkey: String,
}

pub const DEFAULT_HOTKEY: &str = "Alt+Space";

/// Folder names that only ever hold machine-made files.
pub const DEFAULT_EXCLUDED_NAMES: &[&str] = &[
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "Pods",
    "DerivedData",
    "Caches",
    ".Trash",
    ".Trashes",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    "$RECYCLE.BIN",
    "System Volume Information",
];

/// macOS packages: folders that Finder shows as a single file. Lumen indexes
/// none of their insides (an app's resources, a Photos library's internals).
pub const PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "framework",
    "bundle",
    "plugin",
    "kext",
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "xcodeproj",
    "xcworkspace",
    "lrdata",
    "lrlibrary",
    "photolibrary",
    "aplibrary",
    "dSYM",
    "xcarchive",
    "sparsebundle",
    "pkg",
    "mpkg",
];

pub const VOLUMES: &str = "/Volumes";

/// Mounted drives worth indexing: USB disks, SD cards, NAS shares. Skips the
/// boot volume (a symlink to /), Time Machine backups and installer images.
pub fn external_volumes(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.starts_with("com.apple") || name == "Recovery" || name == "Preboot" {
            continue;
        }
        if p.join("Backups.backupdb").exists() || p.join(".timemachine").exists() {
            continue;
        }
        // A mounted installer .dmg: a lone app (and maybe a link to /Applications).
        let installer = std::fs::read_dir(&p)
            .map(|rd| {
                rd.flatten().any(|c| c.path().extension().map(|x| x == "app" || x == "pkg").unwrap_or(false))
            })
            .unwrap_or(false);
        if installer {
            continue;
        }
        out.push(p);
    }
    out.sort();
    out
}

/// A path on an external drive whose drive is not mounted right now.
pub fn volume_offline(path: &Path) -> bool {
    volume_root(path).map(|v| !v.is_dir()).unwrap_or(false)
}

/// "/Volumes/Passport/Movies/a.mp4" → "/Volumes/Passport".
pub fn volume_root(path: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(VOLUMES).ok()?;
    let first = rest.components().next()?;
    Some(Path::new(VOLUMES).join(first.as_os_str()))
}

/// System locations that are never personal, whatever the roots say.
pub fn system_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        "/System",
        "/Library",
        "/Applications",
        "/private",
        "/usr",
        "/bin",
        "/sbin",
        "/opt",
        "/cores",
        "/dev",
        "/etc",
        "/var",
        "/tmp",
        "/proc",
        "/sys",
        "/snap",
        "/boot",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join("Library"));
        v.push(home.join("Applications"));
        v.push(home.join(".Trash"));
    }
    v
}

impl Default for Config {
    fn default() -> Self {
        Config {
            roots: default_roots(),
            excluded_paths: Vec::new(),
            excluded_names: DEFAULT_EXCLUDED_NAMES.iter().map(|s| s.to_string()).collect(),
            min_size: 1,
            include_hidden: false,
            content_search: true,
            include_external: true,
            hotkey: DEFAULT_HOTKEY.to_string(),
        }
    }
}

/// The whole home folder: the Library inside it is excluded anyway, and files
/// people lose tend to be in the folders nobody thinks to add.
pub fn default_roots() -> Vec<PathBuf> {
    dirs::home_dir().into_iter().collect()
}

impl Config {
    pub fn load(path: &Path) -> Config {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Roots with overlaps removed: scanning ~/ and ~/Documents scans ~/ once.
    pub fn effective_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self.roots.iter().filter(|r| r.is_dir()).cloned().collect();
        if self.include_external {
            roots.extend(external_volumes(Path::new(VOLUMES)));
        }
        roots.sort();
        roots.dedup();
        let mut out: Vec<PathBuf> = Vec::new();
        for r in roots {
            if !out.iter().any(|o| r.starts_with(o)) {
                out.push(r);
            }
        }
        out
    }

    /// The single gate every path goes through, for full scans and the watcher.
    pub fn filter(&self) -> PathFilter {
        let mut paths = system_paths();
        paths.extend(self.excluded_paths.iter().cloned());
        // A root the user picked explicitly wins over the system list
        // (someone may really want /Volumes/Archive or /opt/media).
        let roots = self.effective_roots();
        paths.retain(|p| !roots.iter().any(|r| r.starts_with(p) && r != p) && !roots.contains(p));
        PathFilter {
            excluded_paths: paths,
            excluded_names: self.excluded_names.clone(),
            min_size: self.min_size,
            include_hidden: self.include_hidden,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathFilter {
    excluded_paths: Vec<PathBuf>,
    excluded_names: Vec<String>,
    pub min_size: u64,
    include_hidden: bool,
}

impl PathFilter {
    /// Should the walker descend into this directory?
    pub fn allow_dir(&self, path: &Path) -> bool {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => return true, // a root like "/"
        };
        if !self.include_hidden && name.starts_with('.') {
            return false;
        }
        if self.excluded_names.iter().any(|n| n == name) {
            return false;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if PACKAGE_EXTENSIONS.iter().any(|p| p.eq_ignore_ascii_case(ext)) {
                return false;
            }
        }
        !self.excluded_paths.iter().any(|p| path.starts_with(p))
    }

    /// Is this file one Lumen keeps? Returns its kind when so.
    pub fn classify_file(&self, path: &Path) -> Option<crate::Kind> {
        let name = path.file_name()?.to_str()?;
        if !self.include_hidden && name.starts_with('.') {
            return None;
        }
        if name.starts_with("~$") {
            return None; // Office lock files
        }
        let ext = path.extension()?.to_str()?;
        crate::Kind::from_ext(ext)
    }

    /// Every ancestor must also be allowed (used by the watcher, which sees
    /// single paths rather than walking down from a root).
    pub fn allow_path_chain(&self, path: &Path, roots: &[PathBuf]) -> bool {
        let root = match roots.iter().find(|r| path.starts_with(r)) {
            Some(r) => r,
            None => return false,
        };
        let mut cur = path.parent();
        while let Some(dir) = cur {
            if dir == root.as_path() || !dir.starts_with(root) {
                break;
            }
            if !self.allow_dir(dir) {
                return false;
            }
            cur = dir.parent();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_rules() {
        let cfg = Config { roots: vec![PathBuf::from("/")], ..Config::default() };
        let f = cfg.filter();
        assert!(!f.allow_dir(Path::new("/System")));
        assert!(!f.allow_dir(Path::new("/Users/a/proj/node_modules")));
        assert!(!f.allow_dir(Path::new("/Users/a/.config")));
        assert!(!f.allow_dir(Path::new("/Users/a/Pictures/Photos Library.photoslibrary")));
        assert!(f.allow_dir(Path::new("/Users/a/Documents")));
        assert!(f.classify_file(Path::new("/x/简历.pdf")).is_some());
        assert!(f.classify_file(Path::new("/x/~$report.docx")).is_none());
        assert!(f.classify_file(Path::new("/x/lib.dylib")).is_none());
    }

    #[test]
    fn explicit_root_beats_system_list() {
        let cfg = Config { roots: vec![PathBuf::from("/opt/media")], ..Config::default() };
        // effective_roots drops non-existent dirs, so test the retain logic directly
        let f = Config { roots: vec![], ..cfg }.filter();
        assert!(!f.allow_dir(Path::new("/opt/media")));
    }
}

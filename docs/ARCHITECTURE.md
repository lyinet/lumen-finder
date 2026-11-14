# Architecture

```
┌──────────────────────────── Lumen.app ────────────────────────────┐
│  app/ui  (HTML/CSS/JS, no build step)                              │
│    bridge.js ── invoke / listen ──┐      mock index in a browser   │
│                                   ▼                                │
│  app/src-tauri  (Rust, Tauri 2)                                    │
│    commands: snapshot search overview recent largest forgotten     │
│              duplicates start_scan cancel_scan set_roots           │
│              set_excluded open_path reveal quick_look              │
│    events:   scan://progress  scan://done  scan://error            │
│              index://changed  app://summon                         │
│    ⌥Space global shortcut · close = hide · Dock click = show       │
│                                   │                                │
│  crates/lumen-core  (pure Rust, platform-neutral, tested)          │
│    config ─ PathFilter (the one gate for every path)               │
│    scanner ─ ignore::WalkParallel → channel → batched UPSERT       │
│    watcher ─ notify (FSEvents) → 700 ms coalesce → apply_paths     │
│    index  ─ SQLite WAL, files + files_fts (FTS5 trigram)           │
│    search ─ query language → SQL, facets, relevance                │
│    insights ─ overview, recent, largest, forgotten, duplicates     │
└────────────────────────────────────────────────────────────────────┘
        index.db + config.json in ~/Library/Application Support/app.lumen.desktop
```

## Key decisions

**Tauri 2 over a pure-Rust GUI.** The product's edge is feel: blur, springs,
staggered lists, a live radar. WebKit renders that at 120 Hz with tiny
binaries (~8 MB) while every byte of logic stays in Rust. The engine has no
UI dependency, so a native SwiftUI/GPUI shell or a menu-bar extra can reuse it.

**SQLite + FTS5 trigram over Tantivy.** File names are short; what users need
is *substring* matching in any script (`合同` inside `劳动合同扫描件`), which
trigram gives natively. One file, transactional, inspectable, and migrations
are trivial: the index is a cache of the disk, so the fallback for any schema
change is "drop and rescan".

**Generations for incremental scans.** Each scan stamps rows with
`seen_gen`; size/mtime changes bump `changed_gen` and clear the cached hash.
Rows not seen are deleted — except under roots that are configured but not
mounted (unplugged drive). The FTS trigger only fires on name/parent
changes, so rescans don't churn the full-text index.

**One filter for scanner and watcher.** `PathFilter` decides directories
(system paths, `~/Library`, packages like `.app` / `.photoslibrary`, dev
junk, hidden) and files (known personal extensions, no Office lock files).
The watcher checks the whole ancestor chain, so an event deep inside
`node_modules` never sneaks in. A root the user picks explicitly overrides
the system list (`/Volumes/Archive`, `/opt/media`).

**Pinyin column.** `name_py` holds full pinyin + initials for names with
Chinese characters. It costs a few bytes per row and is the feature Chinese
users notice first.

## Performance notes (2-core Linux sandbox, release build)

- 120,701 files / 2,000 folders → 92,984 indexed in 4.4 s; no-change rescan 0.9 s.
- Search over 93k rows: 20–30 ms for trigram queries, ~70 ms for 2-char CJK
  (LIKE fallback). Most of that is the `count(*)` + facet queries.

Next optimisations, in order: cache facet counts per query prefix; cap
`total` at 10k ("10,000+"); a 2-char bigram side table for CJK; skip
unchanged directories on startup scans using directory mtimes + the FSEvents
event id saved at shutdown (`kFSEventStreamEventIdSinceNow` replay).

## Testing

`cargo test --workspace` runs unit tests plus `tests/engine.rs`, which
builds a fake home folder (with Library, node_modules, a Photos library,
dotfiles, lock files) and checks exclusion, Chinese/pinyin search, filters,
relevance, facets, duplicates, incremental add/change/delete and the live
watcher.

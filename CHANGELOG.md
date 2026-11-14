# Changelog

## v0.2.0 — 2026-11-15

**正文搜索 / Full-text search**
- Reads the words inside txt, md, org, rst, tex, csv, tsv, rtf, html, docx, pptx, xlsx, odt, odp, ods, epub and pdf (text layer). Pure Rust, on-device.
- Encodings: UTF-8, UTF-16 (BOM), GB18030/GBK for old Chinese text files.
- Every search word matches the name, pinyin, folder **or** the document text; name hits still rank first. Results that matched inside a file show the matching sentence.
- `name:` filter to search names only. Two-character Chinese words search text by substring; 3+ characters use a trigram FTS5 index.
- Runs after each scan and after live changes, only on files whose size/mtime moved. Files over 80 MB are skipped; up to 120k characters kept per file.
- Schema v2 (additive; v1 indexes upgrade in place).

**External drives**
- Mounted volumes under /Volumes are indexed (boot volume, Time Machine and installer images skipped). Files on an unplugged drive are kept and marked offline; plugging or ejecting a drive triggers a rescan.

**App**
- Duplicates: choose the copy to keep, move the rest to the Trash per group or all at once (two-click confirm).
- Menu bar icon (click to summon; Open / Rescan / Quit).
- Summon shortcut is configurable in Settings, with conflict detection.
- Full Disk Access detection with a deep link to System Settings, in onboarding and Settings.
- Live status shows document reading progress.

**CLI**
- `lumen content` reads document text; `lumen scan` does it automatically. Search output shows snippets and offline files.

**Build**
- CI moved to `macos-latest` (macOS 14 runners retired); tagging `v*` publishes the DMG to GitHub Releases.

## v0.1.0 — 2026-11-15
- First version: scan + live incremental index, pinyin search, filter language, insights, duplicates, Tauri shell, CLI.

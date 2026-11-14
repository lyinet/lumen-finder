<p align="center">
  <img src="docs/assets/hero-en.jpg" alt="Lumen — every file you own, one keystroke away" width="100%">
</p>

<p align="center">
  <a href="README.md">简体中文</a> · <b>English</b>
</p>

<p align="center">
  <img alt="platform" src="https://img.shields.io/badge/macOS-12%2B-111?logo=apple&logoColor=white">
  <img alt="rust" src="https://img.shields.io/badge/Rust-engine-b7410e?logo=rust&logoColor=white">
  <img alt="tauri" src="https://img.shields.io/badge/Tauri-2-24c8db?logo=tauri&logoColor=white">
  <img alt="sqlite" src="https://img.shields.io/badge/SQLite-FTS5-003b57?logo=sqlite&logoColor=white">
  <img alt="license" src="https://img.shields.io/badge/license-MIT-8b7bff">
  <img alt="privacy" src="https://img.shields.io/badge/100%25-offline-ff9a76">
</p>

<h3 align="center">Remember two letters of the name? That's enough.<br>Lumen tells you where it is, in milliseconds.</h3>

<br>

<p align="center">
  <img src="docs/assets/demo.gif" alt="Type, filter by PDF, pick with the keyboard" width="88%">
</p>

<br>

## ✦ New in v0.2: search what your files say

<p align="center">
  <img src="docs/screenshots/fulltext-en.jpg" alt="Full-text search" width="88%">
</p>

- **Full-text search** — reads Word, PDF, Markdown, text, RTF, HTML, PowerPoint, Excel, OpenDocument and EPUB. Type a phrase you remember and get the file, with the matching line. On this Mac only.
- **External drives, even unplugged** — USB disks and SD cards are indexed; unplugged, their files stay searchable, marked "not connected". Plug back in and they resync.
- **Clean up duplicates** — pick the copy to keep, send the rest to the Trash (Put Back works), or clean everything at once.
- **Menu bar icon + custom hotkey** — record any shortcut to replace ⌥ Space; conflicts are caught.
- **Full Disk Access guide** — detected, one click to the right System Settings pane.

## ✦ Why Lumen

> Spotlight searches everything — mail, settings, web suggestions. Finder makes you wait.
> Lumen answers one question only: **where are my files.**

<table>
<tr>
<td width="33%" valign="top">

### ⚡ Scan once, stay in sync
A parallel walk indexes 120k files in **4.4 s**. FSEvents keeps it live; changes made while Lumen was closed are caught on launch.

</td>
<td width="33%" valign="top">

### 🔤 Substring search, any script
Trigram full-text index: any fragment matches, in English, Chinese (with pinyin and initials) or anything else.

</td>
<td width="33%" valign="top">

### 🧹 Only your files
System folders, Library, apps, `.app` / Photos packages, `node_modules`, `.git`, caches and dotfiles are skipped.

</td>
</tr>
<tr>
<td valign="top">

### ⌨️ Keyboard first
`⌥ Space` anywhere, `↑↓` to move, `↵` open, `⌘↵` show in Finder, `Space` Quick Look, `⌘C` copy path.

</td>
<td valign="top">

### 🔍 Filters in one line
`kind:pdf` `in:Downloads` `>100mb` `after:2024-03` `"exact phrase"` — mix freely.

</td>
<td valign="top">

### 🔒 Fully offline
The index never leaves your Mac. No network, no uploads, no telemetry.

</td>
</tr>
</table>

## ✦ A look around

<table>
<tr>
<td width="50%"><img src="docs/screenshots/first.jpg" alt="First run"><p align="center"><sub><b>First run</b> — what's scanned, what's not</sub></p></td>
<td width="50%"><img src="docs/screenshots/scan.jpg" alt="Scanning"><p align="center"><sub><b>Scan radar</b> — every dot is a file found</sub></p></td>
</tr>
<tr>
<td><img src="docs/screenshots/insights.jpg" alt="Insights"><p align="center"><sub><b>Insights</b> — how scattered are your videos?</sub></p></td>
<td><img src="docs/screenshots/dupes.jpg" alt="Duplicates"><p align="center"><sub><b>Duplicates</b> — content fingerprints, sorted by space saved</sub></p></td>
</tr>
</table>

<sub>Screenshots show the UI preview with sample data.</sub>

## ✦ Install

> Until the first signed release, build from source (~3 minutes).

```bash
xcode-select --install
curl https://sh.rustup.rs -sSf | sh
cargo install tauri-cli --version "^2" --locked

git clone https://github.com/lyinet/lumen-finder.git && cd lumen-finder/app/src-tauri
cargo tauri dev                                       # run
cargo tauri build --target universal-apple-darwin     # .dmg for Apple Silicon + Intel
```

To index everything, allow Lumen under **System Settings → Privacy & Security → Full Disk Access**.

Preview the UI without Rust: `cd app/ui && python3 -m http.server 8765`, then open `http://localhost:8765/#first`.

## ✦ Architecture

```
app/ui              vanilla JS + CSS, no build step; mock data in a browser
app/src-tauri       Tauri 2 shell: commands, events, menu bar, hotkey, Trash, vibrancy
crates/lumen-core   engine: exclusion rules · parallel scan · SQLite WAL + FTS5 trigram
                    · pinyin · generational incremental · FSEvents · duplicates (BLAKE3)
                    · text extraction (docx/pptx/xlsx/pdf/epub/…) + content FTS · offline volumes
crates/lumen-cli    command line
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## ✦ Roadmap

- [x] Full scan + live sync, substring/pinyin search, filters, insights, duplicates
- [ ] Signed, notarized DMG + auto-update · Homebrew cask
- [x] Menu bar icon · custom hotkey · Full Disk Access guide <sub>v0.2</sub>
- [x] Offline external drives <sub>v0.2</sub>
- [x] Move duplicates to Trash <sub>v0.2</sub>
- [x] **Content search** for PDF / Word / Markdown / PowerPoint / Excel / EPUB <sub>v0.2</sub>
- [ ] Image and scanned-PDF **OCR** (on-device Apple Vision)
- [ ] Compact menu-bar search panel · launch at login · light theme
- [ ] Smart collections: invoices, contracts, ID photos, screenshots
- [ ] On-device semantic search

<br>

<p align="center"><sub>MIT License · Made with Rust 🦀</sub></p>

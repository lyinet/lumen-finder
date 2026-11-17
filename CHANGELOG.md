# Changelog

## v0.8.0 — unreleased

Polish: first launch, background work, errors with a fix, ranking, custom skips, and a 300 000-file load test.

- **Search while the first scan runs**: once 2 000 files are found, "Start searching now" leaves the scan running in the background (every batch is already committed); a toast says when it finishes. The done screen offers searches built from the user's own files (pinyin initials such as `nzzj → 2024年终总结.key`, plus `kind:pdf >10mb`). Engine: `Index::suggestions()`.
- **Background work stays out of the way** (`lumen_core::pace`): content reading, OCR and embedding run at macOS background priority (`PRIO_DARWIN_BG`, helpers included), and pause on battery (Settings → Background work, on by default), in Low Power Mode, or when the user presses Pause. They resume on their own. The live indicator says why it is paused (`pace://hold` event). The watcher only reads files changed in the last hour, so it never competes with the paused backlog.
- **Errors come with a fix**: Full Disk Access missing or revoked → a banner with "Grant access"; a scan that fails → "Retry"; a failed update → "Retry". A damaged index (failed `quick_check`) is set aside as `*.damaged-<ts>.db` and rebuilt on launch, with a note; Settings → Rebuild index does it on demand (`rebuild_index`).
- **Ranking**: within each relevance tier, files opened from Lumen in the last 30 days come first (most opened, then most recent). An exact name still wins. Schema adds an `opens` table in place.
- **Custom skips**: Settings → "Skip by folder name or type" — folder names skipped anywhere, and `*.ext` patterns for file types (`excluded_names`).
- **Faster on big indexes** (300 000 synthetic files, Linux aarch64 sandbox, slower than Apple silicon): searches with words now scan once instead of three times (page, total and chip counts from one pass, ranked in memory): `合同` 719 → 70 ms, `年终总结` 121 → 52 ms, `nzzj` 125 → 56 ms, `合同 >10mb after:2024` 1 343 → 63 ms. Words with 汉字 skip the pinyin column. Insights, Timeline and collections are cached until the index changes and warmed after each scan. `cargo test -p lumen-core --release --test scale -- --ignored`.

## v0.7.0 — unreleased

- **Raycast extension** (`integrations/raycast`): Search Files and Search Files by Meaning, backed by `lumen --json`. Open, reveal, Quick Look, copy path, open with; iCloud / offline / relevance accessories.
- **Alfred workflow** (`integrations/alfred`, attached to each release as `Lumen.alfredworkflow`): `lf` and `la` keywords via the new `lumen --alfred search|ask` Script Filter output. ⌘↵ reveals.
- **`lumen://` URL scheme**: `search`, `ask` (meaning) and `quick` (the quick panel), each with `?q=`. Links that arrive while Lumen is launching are kept until the window is ready.
- **The `lumen` CLI is bundled** at `Lumen.app/Contents/MacOS/lumen` and finds the embedding helper next to itself; the Homebrew cask links it onto PATH.
- **iCloud Drive**: `~/Library/Mobile Documents/com~apple~CloudDocs` is scanned (Settings → Search powers → iCloud Drive) while the rest of ~/Library stays skipped. Files not on this Mac — `.name.icloud` placeholders (macOS ≤ 13) and dataless files (`SF_DATALESS`, macOS 14+, including Desktop & Documents in iCloud) — are indexed under their real name, marked “iCloud”, and never opened for content, OCR or duplicate fingerprints, so Lumen never triggers a download. Schema adds `files.cloud` in place.
- CI builds the CLI for both architectures and checks that it and the `lumen` URL scheme are inside the bundle.

## v0.6.1 — 2026-11-16

- **Chinese search by meaning**: when this Mac has no Chinese embedding model, the helper now asks macOS for Apple's on-demand Chinese language assets (NLContextualEmbedding, macOS 14+) and waits up to 3 minutes. Only Apple's model is downloaded; no file content leaves the Mac. Set `LUMEN_NO_ASSET_REQUEST=1` to opt out.
- Lumen checks `lumen-ocr embed-info` before every embedding pass and remembers which model each language used; when it changes (a model arrived, or macOS switched models), only that language's vectors are rebuilt.
- Privacy copy names the one-time model download.
- This release is also the first real-Mac test of in-app updates (0.6.0 → 0.6.1).

## v0.6.0 — 2026-11-15

- **Search by meaning (beta)**: describe a file instead of remembering its name ("北海道的雪景视频", "the lease I signed last year"). Every file gets one sentence vector built from its name, folder and first lines of text, from Apple's NaturalLanguage sentence embeddings via the bundled helper (`lumen-ocr embed`): on device, nothing to download. Chinese text uses the Chinese model, everything else the English one; queries are embedded with both. Vectors are stored as i8 (~0.5 KB per file) and searched by brute force in memory. Toggle with the ✦ button in the search bar or ⌘J; a keyword search with no results offers to retry by meaning. Settings → Search powers → Search by meaning. CLI: `lumen ask …`.
- Each language uses the best model the Mac already has: sentence embedding, else contextual embedding (macOS 14+, only if its assets are on disk; Lumen never requests a download), else averaged word vectors. If a Mac has no Chinese model at all, Chinese files simply stay out of meaning search; keyword search is unaffected. `lumen-ocr embed-info` shows what this Mac uses.
- Vectors are built after content reading, newest files first, and kept up to date on every scan (schema adds a `vectors` table; no rescan needed).
- **Homebrew**: `brew tap lyinet/lumen-finder https://github.com/lyinet/lumen-finder && brew install --cask lumen-finder`. CI updates the cask's version and checksum after each release; the cask clears the quarantine flag until releases are notarized.
- CI smoke-tests the embedding helper (related sentences must score closer than unrelated ones).

## v0.5.0 — 2026-11-15

- **Timeline** (sidebar, ⌘4): files grouped by the month they last changed, in local time, with the top kinds per month as a coloured bar; a month opens as a list with kind chips. Engine: `Index::timeline()` / `Index::month()`.
- **Share card**: Insights → Share card draws a 1080×1440 PNG (file count, total size, kind mix, most scattered kind, forgotten files, files touched this week) and saves it to the Desktop.
- **In-app updates** via tauri-plugin-updater: checks `latest.json` on GitHub Releases a minute after launch (Settings → Check for updates, on by default), shows a banner, downloads and verifies the minisign signature, then relaunches. Installs only on click.
- CI signs the update bundle (`TAURI_SIGNING_PRIVATE_KEY` secret) and publishes `Lumen_universal.app.tar.gz`, its `.sig` and `latest.json` with each tagged release.
- Navigation shortcuts are now ⌘1–9. Privacy copy updated: the update check is the only network request.

## v0.4.0 — 2026-11-15

- **Smart collections** (new sidebar page, ⌘3): invoices & receipts, contracts, IDs, screenshots, résumés. Each is a rule over the file name and the words inside documents, PDFs and images; counts, size and last change per collection, and a full list on click.
- **OCR**: a small Swift helper (`lumen-ocr`, Apple Vision + PDFKit, bundled as a sidecar) reads text in JPEG/PNG/HEIC/TIFF/WebP images (20 KB–40 MB) and in PDFs whose text layer is empty, up to 30 pages. Simplified/Traditional Chinese, English, plus Japanese and Korean on macOS 13+. Images are read after documents, newest first, 60 s limit per file. Settings → Search powers → Read text in images. Snippets from images are labelled "In image".
- Scanned PDFs read by v0.3 (empty text) are re-read once through OCR.
- **繁體中文** UI, chosen automatically for zh-TW / zh-HK / zh-MO / zh-Hant.
- CI builds the helper for arm64 + x86_64 and smoke-tests it on a screenshot before packaging.

## v0.3.0 — 2026-11-15

- **Quick panel**: the summon shortcut (and the menu bar icon) opens a Spotlight-style search box centered on the screen with the cursor. ↵ open · ⌘↵ reveal · ⌘Y Quick Look · ⌘C copy path · ⌘O / Tab show all in the main window · Esc or clicking away hides it. Empty query shows recent files. Can be turned off in Settings to summon the main window instead.
- **CJK text index rewritten** (schema v3): document text is indexed as overlapping two-character pieces in a contentless FTS5 table. Two-character Chinese searches 71 → 7.8 ms, index 163 → 98 MB, first read 11.2 → 8.5 s on the 4,000-document benchmark. v2 indexes re-read their documents once after upgrading.
- **Open at login** (LaunchAgent, starts hidden in the menu bar).
- **Appearance**: follow the system, or pin light or dark; applies to both windows and the native vibrancy.
- Changing appearance or panel options no longer triggers a rescan.

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

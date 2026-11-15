# Lumen for Raycast

Two commands, both backed by the `lumen` CLI that ships inside Lumen.app:

- **Search Files** — name, pinyin (`nzzj` → 年终总结), folder or the words inside documents
- **Search Files by Meaning** — describe the file: "北海道的雪景视频", "the lease I signed last year"

↵ open · ⌘↵ show in Finder · ⌘Y Quick Look · ⌘C copy path · ⌘O open with.
Files still in iCloud and files on unplugged drives are marked.

```sh
cd integrations/raycast
npm install
npm run dev        # loads it into Raycast
```

Needs Lumen in /Applications, or set "lumen binary" in the extension's preferences.

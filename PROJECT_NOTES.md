
## v0.6.0 — 2026-11-15
- 本地语义搜索（测试版）：Apple NaturalLanguage 向量，经 `lumen-ocr embed`，i8 量化、内存暴力检索；⌘J / ✦ 切换；CLI `lumen ask`。
- 嵌入后端按语言回退：sentence → contextual（仅本机已有资源，不请求下载）→ 词向量平均。`lumen-ocr embed-info` 查看。
- CI macOS runner：en 为 sentence:512（相关 0.598 / 无关 0.227）；**zh 三种模型都没有**。中文语义搜索是否可用取决于用户 Mac，需真机确认。
- Homebrew cask：本仓库作 tap，CI 在 tag 后更新 version/sha256。
- OCR 继续免费；语义搜索暂免费，与 OCR 一起在 1.0 前定是否入 Pro。

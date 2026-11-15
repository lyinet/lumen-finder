
## v0.6.0 — 2026-11-15
- 本地语义搜索（测试版）：Apple NaturalLanguage 向量，经 `lumen-ocr embed`，i8 量化、内存暴力检索；⌘J / ✦ 切换；CLI `lumen ask`。
- 嵌入后端按语言回退：sentence → contextual（仅本机已有资源，不请求下载）→ 词向量平均。`lumen-ocr embed-info` 查看。
- CI macOS runner：en 为 sentence:512（相关 0.598 / 无关 0.227）；**zh 三种模型都没有**。中文语义搜索是否可用取决于用户 Mac，需真机确认。
- Homebrew cask：本仓库作 tap，CI 在 tag 后更新 version/sha256。
- OCR 继续免费；语义搜索暂免费，与 OCR 一起在 1.0 前定是否入 Pro。

## v0.6.1 — 2026-11-16
- 中文语义搜索：缺模型时助手经 NLContextualEmbedding.requestAssets 请 macOS 下载苹果中文语言资源（最多等 180s，`LUMEN_NO_ASSET_REQUEST=1` 可关）；每次嵌入前读 `embed-info`，按语言存签名（meta `vec_sig_zh/en`），变化时只重建该语言向量。
- 隐私文案改为说明“一次性下载语言模型”。用户首次在真机验证 0.6.0 → 0.6.1 应用内更新。
- 下一大版本计划 v0.7（生态与集成），见 docs/PRODUCT.md。
- 2026-11-16：cbb5775 的 CI 未启动——GitHub 提示账户付款失败或需提高 spending limit（仓库为 private，macOS 分钟按 10 倍计）。v0.6.1 tag 尚未打。
- 发现：仓库 private 时，Release 资产（latest.json、DMG）与 brew tap 对外不可访问，应用内更新和 Homebrew 安装在用户机器上都会失败。待用户决定：改为 public（同时解决 CI 计费），或修复账单并另找公开发布渠道。
- 2026-11-16 用户决定：开发阶段仓库保持 private，正式发布时改 public。v0.6.1 tag 已在本地打在 ab85480，CI 恢复后再推。

## v0.7.0 — 开发中（2026-11-16）
- 范围：生态与集成 + iCloud。Raycast 扩展（integrations/raycast，tsc 通过）、Alfred 工作流（integrations/alfred，CLI `--alfred`）、`lumen://search|ask|quick?q=`（Info.plist + RunEvent::Opened，冷启动经 take_link）、CLI 随 App 打包（externalBin binaries/lumen，cask `binary`）、iCloud 云盘收录（root_exceptions；`.icloud` 占位与 SF_DATALESS 标为 cloud，不读正文/OCR/查重）。
- lumen-core 测试 17 + 9 全过；Tauri 外壳与 CI 新步骤（CLI 双架构、bundle 校验、Alfred 打包）未经 CI 验证（账单阻塞）。
- 截图：docs/screenshots/icloud.jpg、icloud-en.jpg、settings-icloud.jpg。

# Lumen — product plan

## One line
**散落各处的文件，一次全部找回来。** Find any file you own on your Mac by a fragment of its name — in Chinese, pinyin or English — in under 50 ms.

## Who it's for (first)
1. **中文 Mac 用户**：文件名大量中文，Spotlight/Finder 不支持拼音、结果混杂。
2. **内容创作者**：素材、成片、封面、BGM 散在桌面/下载/外置盘。（我们自己就是：14 期视频的素材分布在十几个目录。）
3. **学生与职场人**：合同、发票、论文、简历，"上次那个 PDF 放哪了"。

## Why people switch
| Pain | Spotlight / Finder | Lumen |
|---|---|---|
| 只想找自己的文件 | 混着邮件、设置、网页 | 只有个人文件，系统和缓存全排除 |
| 中文名 | 不支持拼音 | 全拼、首字母、任意子串 |
| 速度 | Finder 搜索要等 | 输入即出结果 |
| 筛选 | 要点好几层菜单 | `kind:pdf >10mb in:下载 after:2024` 一行 |
| 好看 | — | 毛玻璃、弹簧动效、扫描雷达 |

## Launch-day bar (v1.0 must have)
- [x] Full scan + live incremental sync, system exclusions
- [x] Pinyin search, filter language, kind chips with counts
- [x] ⌥Space summon, keyboard-first, Quick Look, reveal
- [x] Insights, duplicates, forgotten files
- [ ] Notarized universal DMG + Sparkle-style auto-update (tauri-plugin-updater)
- [ ] Menu-bar extra with a compact search panel (Spotlight-like, separate window)
- [ ] External drives: index offline volumes, show "on drive X (not connected)"
- [ ] Move duplicates to Trash (with undo), "Clean up" flow from Insights
- [ ] Settings: hotkey rebinding, launch at login, light/dark/auto
- [ ] Onboarding asks for Full Disk Access with a one-click deep link
- [ ] English + 简体中文 + 繁體中文 copy review

## Roadmap after launch
- **v1.1 内容搜索 (Pro)**：PDF/Word/Markdown 正文全文检索、图片 OCR（Apple Vision 免费本地）。"我记得里面写了'违约金'"。
- **v1.2 智能视图**：自动整理"发票""合同""证件照""截图"等智能合集；时间线视图（"去年 11 月我在做什么"）。
- **v1.3 语义搜索**：本地小模型 embedding，"北海道的雪景视频"。全程离线。
- **v1.4 生态**：Raycast / Alfred 扩展、Shortcuts 动作、CLI 已有。
- **Later**：iCloud Drive 占位文件识别、网盘同步目录标注、多台 Mac 索引合并（端到端加密）。

## Business model
- **免费版**：文件名搜索、增量同步、筛选、洞察 —— 足够好用，负责口碑。
- **Pro 买断** ¥68 / $14.99（含 1 年大版本更新）：正文/OCR 搜索、重复清理、外置盘、智能视图、语义搜索。
- 之后进 Setapp 拿订阅分成。Mac App Store 沙盒限制全盘扫描，首发走官网 + Homebrew cask，App Store 版本做"选定文件夹"精简版引流。

## Go-to-market
**中国（首发）**
1. **抖音/小红书**：用《Vibe知识大赏》同款卡通风做 50 秒短片——"你的 Mac 里有几万个文件，散在几千个文件夹"，扫描雷达是天然的视觉钩子。每个功能一条（拼音搜索、重复文件可释放多少空间、两年没动的大文件）。
2. **少数派**投稿首发评测 + 限时早鸟价；**V2EX** 分享创造；**即刻**独立开发者圈子。
3. 开源引擎（MIT）+ App 闭源或同仓库开源，GitHub README 中文优先，争取 Trending。

**海外（第 2–4 周）**
1. Product Hunt（周二发，提前一周预热 hunter），Show HN（强调 Rust + SQLite trigram + FSEvents 的工程细节），r/macapps。
2. 一个 15 秒 GIF：输入 "nzzj" → 秒出《年终总结.key》。

**增长回路**
- 扫描完成页"找到 X 个文件，散落在 Y 个文件夹"做成可分享卡片（一键存图），天然适合发朋友圈/小红书。
- "可释放 X GB"是第二个分享点。

## Metrics
- 激活：首次扫描完成率 > 85%，首扫后 60 秒内第一次搜索 > 70%。
- 留存：D7 每日唤起 ≥ 1 次的用户比例；⌥Space 使用占比。
- 付费：免费→Pro 转化 4–6%。

## Open decisions
- **名字**："Lumen" 是工作名，同名产品不少，上线前需查商标与域名（备选：寻迹 / Trove / Recall）。
- Apple Developer 账号（$99/年）用于签名公证，否则用户首次打开会被 Gatekeeper 拦截。

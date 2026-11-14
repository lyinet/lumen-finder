<p align="center">
  <img src="docs/assets/hero.jpg" alt="Lumen — 散落各处的文件，一次全部找回来" width="100%">
</p>

<p align="center">
  <b>简体中文</b> · <a href="README.en.md">English</a>
</p>

<p align="center">
  <img alt="platform" src="https://img.shields.io/badge/macOS-12%2B-111?logo=apple&logoColor=white">
  <img alt="rust" src="https://img.shields.io/badge/Rust-engine-b7410e?logo=rust&logoColor=white">
  <img alt="tauri" src="https://img.shields.io/badge/Tauri-2-24c8db?logo=tauri&logoColor=white">
  <img alt="sqlite" src="https://img.shields.io/badge/SQLite-FTS5-003b57?logo=sqlite&logoColor=white">
  <img alt="license" src="https://img.shields.io/badge/license-MIT-8b7bff">
  <img alt="privacy" src="https://img.shields.io/badge/100%25-离线-ff9a76">
</p>

<h3 align="center">只记得名字里的两个字？拼音首字母也行。<br>Lumen 在几十毫秒内告诉你，它在哪。</h3>

<br>

<p align="center">
  <img src="docs/assets/demo.gif" alt="输入 合同 → 按 PDF 筛选 → 键盘选择" width="88%">
</p>

<br>

## ✦ 为什么是 Lumen

> Spotlight 什么都搜，结果里混着邮件、设置和网页建议；Finder 搜索要等，也不认拼音。
> Lumen 只回答一个问题：**你自己的文件在哪。**

<table>
<tr>
<td width="33%" valign="top">

### ⚡ 一次扫描，永远同步
并行遍历个人文件夹，12 万文件 **4.4 秒**建好索引。之后通过 FSEvents 实时增量，关机期间的变化启动时自动补齐。

</td>
<td width="33%" valign="top">

### 🀄 懂中文的搜索
任意子串匹配，`合同` 能找到《劳动合同扫描件》；**全拼** `hetong`、**首字母** `nzzj` 都行。

</td>
<td width="33%" valign="top">

### 🧹 只收你的文件
自动排除系统目录、资源库、应用、`.app`/照片图库、`node_modules`、`.git`、缓存与隐藏文件。

</td>
</tr>
<tr>
<td valign="top">

### ⌨️ 键盘优先
`⌥ Space` 全局呼出，`↑↓` 选择，`↵` 打开，`⌘↵` 在访达中显示，`Space` 快速查看，`⌘C` 复制路径。

</td>
<td valign="top">

### 🔍 一行筛选
`kind:pdf` `类型:视频` `in:Downloads` `>100mb` `after:2024-03` `"精确短语"`，自由组合。

</td>
<td valign="top">

### 🔒 完全离线
索引只存在你的 Mac 上。不联网、不上传、不收集任何数据。

</td>
</tr>
</table>

## ✦ 看一看

<table>
<tr>
<td width="50%"><img src="docs/screenshots/first.jpg" alt="首次启动"><p align="center"><sub><b>首次启动</b> · 扫什么、不扫什么，一眼看清</sub></p></td>
<td width="50%"><img src="docs/screenshots/scan.jpg" alt="扫描中"><p align="center"><sub><b>扫描雷达</b> · 每个光点是一类被找到的文件</sub></p></td>
</tr>
<tr>
<td><img src="docs/screenshots/insights.jpg" alt="洞察"><p align="center"><sub><b>洞察</b> · 你的视频散落在多少个文件夹里？</sub></p></td>
<td><img src="docs/screenshots/dupes.jpg" alt="重复文件"><p align="center"><sub><b>重复文件</b> · 内容指纹比对，按可释放空间排序</sub></p></td>
</tr>
</table>

<sub>截图为界面预览（模拟数据）。</sub>

## ✦ 搜索语法

| 写法 | 含义 |
|---|---|
| `合同` `hetong` `ht` | 名称或文件夹包含；拼音全拼 / 首字母 |
| `kind:pdf` `类型:音乐` | 按类型：文档 · 表格 · 演示 · PDF · 电子书 · 音乐 · 视频 · 图片 · 压缩包 · 设计 |
| `ext:key` | 按扩展名 |
| `in:Downloads` | 路径中包含 |
| `>100mb` `<2gb` | 按大小 |
| `after:2024-03` `before:2025` | 按修改时间 |
| `"meeting notes"` | 精确短语 |

## ✦ 安装

> 首个签名版本发布前，请从源码构建（约 3 分钟）。

```bash
xcode-select --install
curl https://sh.rustup.rs -sSf | sh
cargo install tauri-cli --version "^2" --locked

git clone https://github.com/lyinet/lumen-finder.git && cd lumen-finder/app/src-tauri
cargo tauri dev                                       # 开发运行
cargo tauri build --target universal-apple-darwin     # 打包 .dmg（Apple Silicon + Intel）
```

想一次扫全，请在 **系统设置 → 隐私与安全性 → 完全磁盘访问权限** 中允许 Lumen。

<details>
<summary><b>不装 Rust，先在浏览器里看界面</b></summary>

```bash
cd app/ui && python3 -m http.server 8765
# 打开 http://localhost:8765/#first   （#insights  #dupes  #q=合同）
```
</details>

<details>
<summary><b>命令行</b>（与 App 共用索引）</summary>

```bash
cargo run -p lumen-cli -- scan ~
cargo run -p lumen-cli -- search 合同 kind:pdf
cargo run -p lumen-cli -- stats
cargo run -p lumen-cli -- dupes
```
</details>

## ✦ 架构

```
app/ui              原生 JS + CSS，零构建；浏览器中自动使用模拟数据
app/src-tauri       Tauri 2 外壳：命令、事件、⌥Space、毛玻璃窗口
crates/lumen-core   引擎：排除规则 · 并行扫描 · SQLite WAL + FTS5 trigram
                    · 拼音索引 · 增量（generation）· FSEvents 监听 · 重复文件（BLAKE3）
crates/lumen-cli    命令行
```

设计取舍与性能数据见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## ✦ 路线图

- [x] 全盘扫描 + 实时增量、中文/拼音搜索、筛选语法、洞察、重复文件
- [ ] 签名公证 DMG + 自动更新 · Homebrew cask
- [ ] 菜单栏迷你搜索窗
- [ ] 外置硬盘离线索引（"在 移动硬盘 上，未连接"）
- [ ] 重复文件一键移到废纸篓（可撤销）
- [ ] **正文搜索** PDF / Word / Markdown，图片 **OCR**（本地 Apple Vision）
- [ ] 智能合集：发票、合同、证件照、截图
- [ ] 本地语义搜索："北海道的雪景视频"

完整计划见 [docs/PRODUCT.md](docs/PRODUCT.md)。

## ✦ 参与

欢迎 Issue 和 PR。引擎测试在 Linux / macOS 都能跑：

```bash
cargo test --workspace
```

<br>

<p align="center"><sub>MIT License · Made with Rust 🦀 and a lot of 毛玻璃</sub></p>

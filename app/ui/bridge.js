// One interface to the Rust side. Inside the app it calls Tauri; opened in a
// plain browser (design work, screenshots, demos) it runs on a mock index so
// every screen can be previewed without a Mac build.

const tauri = window.__TAURI__

export const isApp = !!tauri

function realBridge() {
  const { invoke } = tauri.core
  const { listen } = tauri.event
  return {
    snapshot: () => invoke('snapshot'),
    search: req => invoke('search', { req }),
    semantic: req => invoke('semantic', { req }),
    overview: () => invoke('overview'),
    recent: limit => invoke('recent', { limit }),
    largest: limit => invoke('largest', { limit }),
    forgotten: limit => invoke('forgotten', { limit }),
    duplicates: () => invoke('duplicates'),
    startScan: () => invoke('start_scan'),
    cancelScan: () => invoke('cancel_scan'),
    setRoots: roots => invoke('set_roots', { roots }),
    setExcluded: paths => invoke('set_excluded', { paths }),
    open: path => invoke('open_path', { path }),
    reveal: path => invoke('reveal', { path }),
    quickLook: path => invoke('quick_look', { path }),
    trash: paths => invoke('trash_paths', { paths }),
    setOptions: opts => invoke('set_options', opts),
    setHotkey: hotkey => invoke('set_hotkey', { hotkey }),
    openFdaSettings: () => invoke('open_fda_settings'),
    collections: () => invoke('collections'),
    collection: (id, limit) => invoke('collection', { id, limit }),
    timeline: () => invoke('timeline'),
    month: (month, kind, limit) => invoke('month', { month, kind: kind || null, limit }),
    saveShareCard: png => invoke('save_share_card', { png: Array.from(png) }),
    checkUpdate: () => invoke('check_update'),
    installUpdate: () => invoke('install_update'),
    quickResize: height => invoke('quick_resize', { height }),
    quickHide: () => invoke('quick_hide'),
    showInMain: query => invoke('show_in_main', { query }),
    takeLink: () => invoke('take_link'),
    pickFolder: async () => tauri.dialog ? tauri.dialog.open({ directory: true, multiple: false }) : null,
    on: (event, cb) => listen(event, e => cb(e.payload)),
  }
}

// ---------------------------------------------------------------- mock ----
function mockBridge() {
  const HOME = '/Users/lin'
  const KINDS = ['document', 'spreadsheet', 'presentation', 'pdf', 'ebook', 'audio', 'video', 'image', 'archive', 'design']
  const EXT = { document: ['docx', 'md', 'pages', 'txt'], spreadsheet: ['xlsx', 'numbers', 'csv'], presentation: ['key', 'pptx'],
    pdf: ['pdf'], ebook: ['epub', 'mobi'], audio: ['flac', 'mp3', 'm4a'], video: ['mp4', 'mov', 'mkv'],
    image: ['heic', 'jpg', 'png'], archive: ['zip', 'dmg', '7z'], design: ['psd', 'fig', 'sketch'] }
  const NAMES = {
    document: ['2024年终总结', '产品需求文档 v3', '读书笔记', '周报-第46周', '面试准备清单', 'Meeting notes Q3', '旅行计划-京都', '论文初稿', 'README', '租房合同扫描说明'],
    spreadsheet: ['家庭预算2025', '报价单-客户A', '销售数据汇总', 'Invoice tracker', '健身记录', '年度KPI'],
    presentation: ['融资路演', '季度复盘', 'Product launch deck', '毕业答辩', '品牌提案'],
    pdf: ['劳动合同', '个人简历-2026', 'invoice-apple-store', '服务协议-签字版', '护照扫描件', '发票-11月', 'Tax return 2024', '体检报告', '房屋租赁合同', 'Boarding pass', '保险单'],
    ebook: ['三体', '人类简史', 'Atomic Habits', '原则', '置身事内'],
    audio: ['晴天 - 周杰伦', 'Clair de Lune', '播客录音-第12期', '会议录音 1104', 'Blinding Lights', '夜曲'],
    video: ['婚礼花絮', '北海道旅行 4K', '宝宝第一次走路', 'Keynote 录屏', '毕业典礼', 'drone-shot-coast'],
    image: ['IMG_2041', '全家福', '证件照-白底', 'Screenshot 2026-10-03', '樱花', 'logo-final-final', 'Screenshot 2026-11-12', '截屏2026-09-30', '收据-咖啡'],
    archive: ['项目备份', 'photos-2019', '素材包', 'installer'],
    design: ['海报-终版', 'App UI v2', '名片设计', 'Brand kit'],
  }
  const FOLDERS = ['Documents/工作', 'Documents/个人', 'Desktop', 'Downloads', 'Documents/工作/客户A/合同', 'Movies/旅行',
    'Music/iTunes/Music', 'Pictures/导出', 'Documents/学习/研究生', 'Desktop/待整理', 'Dropbox/共享', 'Documents/财务/2024']
  let seed = 7
  const rnd = () => (seed = (seed * 16807) % 2147483647) / 2147483647
  const pick = a => a[Math.floor(rnd() * a.length)]
  const now = Math.floor(Date.now() / 1000)
  const SZ = { video: 9e8, audio: 9e6, image: 4e6, archive: 3e8, design: 1.2e8, presentation: 2.4e7, pdf: 3e6, ebook: 4e6, document: 2e5, spreadsheet: 4e5 }
  const files = []
  for (let i = 0; i < 2600; i++) {
    const kind = KINDS[Math.min(9, Math.floor(Math.pow(rnd(), 1.4) * 10))]
    const base = pick(NAMES[kind]) + (rnd() < .3 ? ` (${1 + Math.floor(rnd() * 4)})` : '')
    const ext = pick(EXT[kind])
    const parent = `${HOME}/${pick(FOLDERS)}`
    files.push({ id: i + 1, name: `${base}.${ext}`, parent, path: `${parent}/${base}.${ext}`, ext, kind,
      size: Math.floor(SZ[kind] * (0.05 + rnd() * 2)), mtime: now - Math.floor(Math.pow(rnd(), 2.2) * 6 * 365 * 86400) })
  }
  // Some documents carry text, so content search can be previewed.
  const TEXTS = [
    ['document', '租房合同扫描说明', 'md', 'Documents/个人', '房屋租赁合同，租期三年，自 2024 年 3 月 1 日起，月租金 6500 元，押一付三。'],
    ['pdf', '2023 年度审计报告', 'pdf', 'Documents/财务/2024', '本年度营业收入同比增长 42%，主要来自海外市场；合同期限内无重大违约事项。'],
    ['presentation', '季度复盘', 'pptx', 'Documents/工作', 'Q3 复盘：用户留存提升到 61%，下季度重点是会员体系和合同续签。'],
    ['document', '读书笔记 - 置身事内', 'docx', 'Documents/学习/研究生', '地方政府的土地财政与招商引资，决定了过去二十年的城市化速度。'],
    ['pdf', 'Quarterly report Q3', 'pdf', 'Downloads', 'Quarterly revenue grew 42% year over year, led by subscriptions in Japan and Korea.'],
    ['spreadsheet', '家庭预算2025', 'xlsx', 'Documents/个人', '房贷 8200 · 物业 460 · 孩子兴趣班 1800 · 旅行基金 3000 · 合同保险 920'],
  ]
  for (const [kind, base, ext, folder, text] of TEXTS) {
    const parent = `${HOME}/${folder}`
    files.push({ id: files.length + 1, name: `${base}.${ext}`, parent, path: `${parent}/${base}.${ext}`, ext, kind, size: 380000, mtime: now - 86400 * (3 + files.length % 40), text })
  }
  for (const [name, kind, ext] of [['北海道旅行 原片', 'video', 'mov'], ['相册备份 2019', 'archive', 'zip'], ['合同扫描 归档', 'pdf', 'pdf']]) {
    const parent = '/Volumes/Passport/归档'
    files.push({ id: files.length + 1, name: `${name}.${ext}`, parent, path: `${parent}/${name}.${ext}`, ext, kind, size: 2.4e9, mtime: now - 86400 * 400, offline: true })
  }
  // iCloud Drive files that are not downloaded (v0.7): kept, never read.
  for (const [name, kind, ext] of [['季度预算-终版', 'spreadsheet', 'numbers'], ['2025 家庭旅行照片', 'archive', 'zip'], ['年终总结-定稿', 'presentation', 'key']]) {
    const parent = `${HOME}/Library/Mobile Documents/com~apple~CloudDocs/工作`
    files.push({ id: files.length + 1, name: `${name}.${ext}`, parent, path: `${parent}/${name}.${ext}`, ext, kind, size: 0, mtime: now - 86400 * 21, cloud: true })
  }
  // A few files that only match by meaning, for the v0.6 preview.
  for (const [name, kind, ext, folder, days] of [
    ['IMG_4471 札幌雪祭', 'image', 'heic', 'Pictures/导出', 330], ['DJI_0032', 'video', 'mp4', 'Movies/旅行/2025-01', 320],
    ['niseko-powder-day1', 'video', 'mov', 'Movies/旅行/2025-01', 322], ['小樽运河夜景', 'image', 'jpg', 'Pictures/导出', 318],
    ['冬季行程-最终版', 'document', 'pages', 'Documents/个人', 360], ['ski-rental-receipt', 'pdf', 'pdf', 'Downloads', 321]]) {
    const parent = `${HOME}/${folder}`
    files.push({ id: files.length + 1, name: `${name}.${ext}`, parent, path: `${parent}/${name}.${ext}`, ext, kind, size: kind === 'video' ? 1.8e9 : kind === 'image' ? 5.2e6 : 2.1e5, mtime: now - 86400 * days })
  }
  let scanned = !location.hash.includes('first')
  const listeners = {}
  const COLS = [['invoices', /发票|invoice|收据/i], ['contracts', /合同|协议|contract/i], ['ids', /证件|护照|passport|身份证/i], ['screenshots', /^(Screenshot|截屏)/], ['resumes', /简历|resume/i]]
  const opts = { content_search: true, ocr: true, include_external: true, hotkey: 'Alt+Space', quick_panel: true, launch_at_login: false, theme: 'auto', semantic: true }
  const emit = (e, p) => (listeners[e] || []).forEach(cb => cb(p))
  const ALIAS = { 文档: 'document', 音乐: 'audio', 视频: 'video', 图片: 'image', 照片: 'image', music: 'audio', doc: 'document', photo: 'image' }

  function query(req) {
    const t0 = performance.now()
    let terms = [], kinds = [...(req.kinds || [])]
    for (const tok of (req.query || '').trim().split(/\s+/).filter(Boolean)) {
      const m = tok.match(/^(kind|类型):(.+)$/i)
      if (m) { const k = ALIAS[m[2]] || m[2]; if (KINDS.includes(k)) kinds.push(k); continue }
      terms.push(tok.toLowerCase())
    }
    const inName = (f, t) => f.name.toLowerCase().includes(t) || f.parent.toLowerCase().includes(t)
    const base = files.filter(f => terms.every(t => inName(f, t) || (f.text || '').toLowerCase().includes(t)))
    const kind_counts = {}
    base.forEach(f => kind_counts[f.kind] = (kind_counts[f.kind] || 0) + 1)
    let hits = kinds.length ? base.filter(f => kinds.includes(f.kind)) : base
    const s = req.sort || 'relevance'
    const score = f => terms.length ? (f.name.toLowerCase().startsWith(terms[0]) ? 0 : f.name.toLowerCase().includes(terms[0]) ? 1 : inName(f, terms[0]) ? 2 : 3) : 0
    hits = [...hits].sort((a, b) => s === 'largest' ? b.size - a.size : s === 'name' ? a.name.localeCompare(b.name) :
      s === 'relevance' ? (score(a) - score(b)) || (b.mtime - a.mtime) : b.mtime - a.mtime)
    const off = req.offset || 0
    const snip = f => {
      if (!f.text || terms.every(t => f.name.toLowerCase().includes(t))) return undefined
      const t = terms.find(t => f.text.toLowerCase().includes(t)); if (!t) return undefined
      const i = f.text.toLowerCase().indexOf(t), a = Math.max(0, i - 18)
      return (a ? '…' : '') + f.text.slice(a, i + t.length + 36) + (i + t.length + 36 < f.text.length ? '…' : '')
    }
    const page = hits.slice(off, off + (req.limit || 60)).map(f => ({ ...f, offline: !!f.offline, cloud: !!f.cloud, snippet: snip(f), text: undefined }))
    return { hits: page, total: hits.length, kind_counts, took_us: Math.round((performance.now() - t0) * 1000) + 300 }
  }

  // Stand-in for Apple's sentence model: concepts several words point to.
  const CONCEPTS = [
    [/雪|snow|ski|powder|niseko|滑雪|冬/i, 1], [/北海道|札幌|小樽|hokkaido|niseko|日本|japan/i, 1], [/视频|影片|video|录屏|mov|mp4|dji/i, .6],
    [/旅行|行程|trip|travel/i, .5], [/合同|协议|contract|lease|租/i, 1], [/报价|quote|发票|invoice|收据|receipt/i, .9],
    [/照片|photo|全家福|img|pictures/i, .6], [/毕业|graduation|答辩/i, 1], [/预算|budget|开销|money/i, 1], [/路演|融资|pitch|deck/i, 1],
  ]
  const DATES = { 2025: /DJI_0032|niseko|小樽|札幌|ski-rental/, 去年: /DJI_0032|niseko|小樽|札幌|ski-rental|冬季/ }
  const vec = t => CONCEPTS.map(([re, w]) => re.test(t) ? w : 0)
  function semantic(req) {
    const t0 = performance.now(), qv = vec(req.query)
    const qn = Math.hypot(...qv) || 1
    const hits = []
    for (const f of files) {
      const text = `${f.name} ${f.parent.split('/').slice(-2).join(' ')} ${f.ext}`
      const v = vec(text), n = Math.hypot(...v)
      if (!n) continue
      let s = v.reduce((a, x, i) => a + x * qv[i], 0) / (n * qn)
      for (const [w, re] of Object.entries(DATES)) if (req.query.includes(w) && re.test(f.name)) s += .08
      if (s > .58 && (!req.kinds?.length || req.kinds.includes(f.kind))) hits.push({ ...f, text: undefined, score: Math.min(.97, s * .93) })
    }
    hits.sort((a, b) => b.score - a.score || b.mtime - a.mtime)
    return { hits: hits.slice(0, req.limit || 40), indexed: files.length, took_us: Math.round((performance.now() - t0) * 1000) + 9000 }
  }

  return {
    semantic: async req => semantic(req),
    snapshot: async () => ({
      status: { files: scanned ? files.length : 0, bytes: files.reduce((a, f) => a + f.size, 0), last_scan_at: now - 1800, last_scan_ms: 41200, generation: scanned ? 3 : 0 },
      config: { roots: [HOME], excluded_paths: [`${HOME}/Documents/虚拟机`], excluded_names: [], min_size: 1, include_hidden: false, ...opts },
      scanning: false, firstRun: !scanned, home: HOME, reading: false, fullDiskAccess: !location.hash.includes('nofda'), version: '0.7.0',
      semanticReady: true, semanticFiles: files.length,
    }),
    search: async req => query(req),
    overview: async () => {
      const by = {}
      files.forEach(f => { const k = by[f.kind] ||= { kind: f.kind, files: 0, bytes: 0, set: new Set() }; k.files++; k.bytes += f.size; k.set.add(f.parent) })
      const kinds = Object.values(by).map(k => ({ kind: k.kind, files: k.files * 41, bytes: k.bytes * 41, folders: k.set.size * 37 })).sort((a, b) => b.bytes - a.bytes)
      const fold = {}
      files.forEach(f => { const x = fold[f.parent] ||= { path: f.parent, files: 0, bytes: 0 }; x.files++; x.bytes += f.size })
      return { kinds, top_folders: Object.values(fold).sort((a, b) => b.files - a.files).slice(0, 6).map(f => ({ ...f, files: f.files * 41 })),
        added_7d: 214, changed_7d: 389, forgotten_files: 132, forgotten_bytes: 286e9 }
    },
    recent: async n => [...files].sort((a, b) => b.mtime - a.mtime).slice(0, n || 40),
    largest: async n => [...files].sort((a, b) => b.size - a.size).slice(0, n || 40),
    forgotten: async n => files.filter(f => f.mtime < now - 2 * 365 * 86400 && f.size > 5e7).sort((a, b) => b.size - a.size).slice(0, n || 40),
    duplicates: async () => {
      await new Promise(r => setTimeout(r, 900))
      const groups = []
      for (let i = 0; i < 8; i++) {
        const f = files.filter(x => x.size > 5e7)[i * 3]
        const copies = 2 + (i % 3)
        groups.push({ size: f.size, wasted: f.size * (copies - 1), files: Array.from({ length: copies }, (_, j) => ({ ...f, id: f.id * 100 + j, parent: `${HOME}/${FOLDERS[(i + j * 3) % FOLDERS.length]}`, path: `${HOME}/${FOLDERS[(i + j * 3) % FOLDERS.length]}/${f.name}` })) })
      }
      return groups.sort((a, b) => b.wasted - a.wasted)
    },
    startScan: async () => {
      const t0 = Date.now(), total = 128402, D = 6500
      const dirs = FOLDERS.map(f => `${HOME}/${f}`)
      const tick = setInterval(() => {
        const p = Math.min(1, (Date.now() - t0) / D), e = 1 - Math.pow(1 - p, 2.2)
        emit('scan://progress', { phase: p < 1 ? 'walking' : 'finishing', files_found: Math.floor(total * e), dirs: Math.floor(9812 * e), bytes_found: Math.floor(1.9e12 * e), elapsed_ms: Date.now() - t0, current: pick(dirs) + '/' + pick(NAMES.document) })
        if (p >= 1) {
          clearInterval(tick); scanned = true
          setTimeout(() => emit('scan://done', { generation: 1, full: true, cancelled: false, files: total, bytes: 1.9e12, added: total, changed: 0, removed: 0, dirs: 9812, elapsed_ms: D }), 400)
          let d = 0; const T2 = 6120
          const rt = setInterval(() => { d = Math.min(T2, d + 410); emit('content://progress', { done: d, total: T2, current: '', finished: d >= T2 })
            if (d >= T2) { clearInterval(rt); emit('content://done', { read: T2, with_text: 5874, failed: 12, elapsed_ms: 9100, cancelled: false }) } }, 120)
        }
      }, 90)
    },
    cancelScan: async () => {},
    trash: async paths => { await new Promise(r => setTimeout(r, 350)); return { count: paths.length, bytes: paths.length * 6.4e8 } },
    setOptions: async o => {
      const map = { ocr: 'ocr', contentSearch: 'content_search', includeExternal: 'include_external', quickPanel: 'quick_panel', launchAtLogin: 'launch_at_login', theme: 'theme', autoUpdate: 'auto_update', semantic: 'semantic', includeIcloud: 'include_icloud' }
      for (const [k, v] of Object.entries(o)) if (map[k]) opts[map[k]] = v
      return opts
    },
    collections: async () => COLS.map(([id, re]) => { const m = files.filter(f => re.test(f.name)); return { id, files: m.length, bytes: m.reduce((a, f) => a + f.size, 0), latest: Math.max(0, ...m.map(f => f.mtime)) } }),
    collection: async id => { const re = Object.fromEntries(COLS)[id]; return files.filter(f => re.test(f.name)).sort((a, b) => b.mtime - a.mtime) },
    timeline: async () => {
      const ym = t => { const d = new Date(t * 1000); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}` }
      const by = {}
      files.forEach(f => { const m = by[ym(f.mtime)] ||= { month: ym(f.mtime), files: 0, bytes: 0, k: {} }; m.files += 37; m.bytes += f.size * 37; m.k[f.kind] = (m.k[f.kind] || 0) + 37 })
      return Object.values(by).sort((a, b) => b.month.localeCompare(a.month))
        .map(m => ({ month: m.month, files: m.files, bytes: m.bytes, kinds: Object.entries(m.k).sort((a, b) => b[1] - a[1]).slice(0, 4).map(([kind, files]) => ({ kind, files })) }))
    },
    month: async (month, kind) => {
      const ym = t => { const d = new Date(t * 1000); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}` }
      return files.filter(f => ym(f.mtime) === month && (!kind || f.kind === kind)).sort((a, b) => b.mtime - a.mtime)
    },
    saveShareCard: async () => '/Users/lin/Desktop/Lumen.png',
    checkUpdate: async () => location.hash.includes('update') ? { version: '0.7.0', current: '0.6.1', notes: '' } : null,
    installUpdate: async () => { for (let p = 0; p <= 1; p += 0.1) { emit('update-progress', p); await new Promise(r => setTimeout(r, 120)) } },
    quickResize: async () => {}, quickHide: async () => {}, showInMain: async () => {},
    // #link=ask:北海道 previews a lumen:// link arriving.
    takeLink: async () => { const m = location.hash.match(/^#link=(search|ask):(.*)$/); return m ? { mode: m[1], query: decodeURIComponent(m[2]) } : null },
    setHotkey: async k => { if (k === 'Cmd+Space') throw 'taken'; opts.hotkey = k; return opts },
    openFdaSettings: async () => {},
    setRoots: async roots => ({ roots }),
    setExcluded: async p => ({ excluded_paths: p }),
    open: async () => {}, reveal: async () => {}, quickLook: async () => {},
    pickFolder: async () => `${HOME}/Projects`,
    on: async (e, cb) => { (listeners[e] ||= []).push(cb) },
  }
}

export const api = isApp ? realBridge() : mockBridge()

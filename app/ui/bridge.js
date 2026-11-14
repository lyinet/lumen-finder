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
    pdf: ['劳动合同', '护照扫描件', '发票-11月', 'Tax return 2024', '体检报告', '房屋租赁合同', 'Boarding pass', '保险单'],
    ebook: ['三体', '人类简史', 'Atomic Habits', '原则', '置身事内'],
    audio: ['晴天 - 周杰伦', 'Clair de Lune', '播客录音-第12期', '会议录音 1104', 'Blinding Lights', '夜曲'],
    video: ['婚礼花絮', '北海道旅行 4K', '宝宝第一次走路', 'Keynote 录屏', '毕业典礼', 'drone-shot-coast'],
    image: ['IMG_2041', '全家福', '证件照-白底', 'Screenshot 2026-10-03', '樱花', 'logo-final-final'],
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
  let scanned = !location.hash.includes('first')
  const listeners = {}
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
    const base = files.filter(f => terms.every(t => f.name.toLowerCase().includes(t) || f.parent.toLowerCase().includes(t)))
    const kind_counts = {}
    base.forEach(f => kind_counts[f.kind] = (kind_counts[f.kind] || 0) + 1)
    let hits = kinds.length ? base.filter(f => kinds.includes(f.kind)) : base
    const s = req.sort || 'relevance'
    const score = f => terms.length ? (f.name.toLowerCase().startsWith(terms[0]) ? 0 : f.name.toLowerCase().includes(terms[0]) ? 1 : 2) : 0
    hits = [...hits].sort((a, b) => s === 'largest' ? b.size - a.size : s === 'name' ? a.name.localeCompare(b.name) :
      s === 'relevance' ? (score(a) - score(b)) || (b.mtime - a.mtime) : b.mtime - a.mtime)
    const off = req.offset || 0
    return { hits: hits.slice(off, off + (req.limit || 60)), total: hits.length, kind_counts, took_us: Math.round((performance.now() - t0) * 1000) + 300 }
  }

  return {
    snapshot: async () => ({
      status: { files: scanned ? files.length : 0, bytes: files.reduce((a, f) => a + f.size, 0), last_scan_at: now - 1800, last_scan_ms: 41200, generation: scanned ? 3 : 0 },
      config: { roots: [HOME], excluded_paths: [`${HOME}/Documents/虚拟机`], excluded_names: [], min_size: 1, include_hidden: false },
      scanning: false, firstRun: !scanned, home: HOME,
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
        }
      }, 90)
    },
    cancelScan: async () => {},
    setRoots: async roots => ({ roots }),
    setExcluded: async p => ({ excluded_paths: p }),
    open: async () => {}, reveal: async () => {}, quickLook: async () => {},
    pickFolder: async () => `${HOME}/Projects`,
    on: async (e, cb) => { (listeners[e] ||= []).push(cb) },
  }
}

export const api = isApp ? realBridge() : mockBridge()

import { api, isApp } from './bridge.js'
import { T } from './i18n.js'
import { ICON, esc, bytes, ago, highlight, kc, applyTheme, COLLECTIONS } from './common.js'

if (!isApp) document.body.classList.add('browser')

// ------------------------------------------------------------ helpers ----
const $ = (s, el = document) => el.querySelector(s)
const h = (html) => { const t = document.createElement('template'); t.innerHTML = html.trim(); return t.content.firstElementChild }
const KINDS = ['document', 'pdf', 'spreadsheet', 'presentation', 'image', 'video', 'audio', 'ebook', 'design', 'archive']
const fmt = n => Number(n).toLocaleString()
function shortPath(p) { return state.home && p.startsWith(state.home) ? '~' + p.slice(state.home.length) : p }

/** Count up to a number with an ease-out, like a odometer settling. */
function tween(el, to, { dur = 900, format = fmt } = {}) {
  const from = Number(el.dataset.v || 0); el.dataset.v = to
  if (from === to) { el.textContent = format(to); return }
  const t0 = performance.now()
  const step = now => {
    const p = Math.min(1, (now - t0) / dur), e = 1 - Math.pow(1 - p, 3)
    el.textContent = format(Math.round(from + (to - from) * e))
    if (p < 1) requestAnimationFrame(step)
  }
  requestAnimationFrame(step)
}



// ------------------------------------------------------------ state ------
const state = {
  view: 'search', home: '', snapshot: null, scanning: false,
  query: '', kinds: new Set(), sort: 'relevance',
  hits: [], total: 0, counts: {}, took: 0, sel: 0, offset: 0, more: false, seq: 0,
}

// ------------------------------------------------------------ nav --------
const NAV = [
  ['search', ICON.search, () => T.nav_search],
  ['recent', ICON.recent, () => T.nav_recent],
  ['collections', ICON.collections, () => T.nav_collections],
  ['timeline', ICON.timeline, () => T.nav_timeline],
  ['insights', ICON.insights, () => T.nav_insights],
  'sep',
  ['dupes', ICON.dupes, () => T.nav_dupes],
  ['forgotten', ICON.forgotten, () => T.nav_forgotten],
  ['large', ICON.large, () => T.nav_large],
  'sep',
  ['settings', ICON.settings, () => T.nav_settings],
]
function renderNav() {
  const nav = $('#nav'); nav.innerHTML = '<div class="nav-pill"></div>'
  let n = 0
  for (const item of NAV) {
    if (item === 'sep') { nav.append(h('<div class="sep"></div>')); continue }
    const [id, icon, label] = item; n++
    const b = h(`<button data-view="${id}">${icon}<span>${label()}</span><span class="kbd">⌘${n}</span></button>`)
    b.onclick = () => go(id)
    nav.append(b)
  }
  movePill()
}
function movePill() {
  const nav = $('#nav'), pill = $('.nav-pill', nav)
  nav.querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.view === state.view))
  const on = $(`button[data-view="${state.view}"]`, nav)
  if (on && pill) pill.style.transform = `translateY(${on.offsetTop}px)`
}
function go(view) {
  if (state.view === view && view !== 'search' && !(view === 'collections' && state.collection) && !(view === 'timeline' && state.month)) return
  if (view === 'collections' && state.view === 'collections') state.collection = null
  if (view === 'timeline' && state.view === 'timeline') state.month = null
  state.view = view; movePill(); render()
}

// ------------------------------------------------------------ views ------
const VIEWS = { timeline: viewTimeline, collections: viewCollections, search: viewSearch, recent: () => viewList('recent'), large: () => viewList('large'), forgotten: () => viewList('forgotten'), insights: viewInsights, dupes: viewDupes, settings: viewSettings }
function render() {
  const v = $('#view'); v.innerHTML = ''; v.classList.remove('view-enter'); void v.offsetWidth; v.classList.add('view-enter')
  VIEWS[state.view](v)
}

const volName = p => (p.match(/^\/Volumes\/([^/]+)/) || [])[1]
function rowHTML(f, terms = [], i = 0, extra = '') {
  const vol = f.offline ? volName(f.path) : null
  const sub = f.snippet
    ? `<div class="snip"><b>${f.kind === 'image' || (f.kind === 'pdf' && f.ocr) ? T.in_image : T.in_text}</b><span>${highlight(f.snippet, terms)}</span></div>`
    : `<div class="path"><span>${esc(shortPath(f.parent))}</span></div>`
  return `<div class="row ${f.snippet ? 'has-snip' : ''} ${vol ? 'offline' : ''}" data-path="${esc(f.path)}" data-i="${i}" style="animation-delay:${Math.min(i, 14) * 18}ms">
    <div class="tile" style="--c:${kc(f.kind)}">${ICON.k[f.kind] || ICON.k.document}<em>${esc(f.ext).slice(0, 4)}</em></div>
    <div class="txt"><div class="name">${highlight(f.name, terms)}${vol ? `<span class="badge">${ICON.drive}${esc(T.offline(vol))}</span>` : ''}</div>${sub}</div>
    <div class="size">${bytes(f.size)}</div><div class="date">${ago(f.mtime)}</div>${extra}</div>`
}

function bindRows(container, getList) {
  container.addEventListener('dblclick', e => { const r = e.target.closest('.row'); if (r) act('open', r.dataset.path) })
  container.addEventListener('click', e => {
    const r = e.target.closest('.row'); if (!r || !container.classList.contains('results')) return
    state.sel = Number(r.dataset.i); moveSel()
  })
  container.addEventListener('contextmenu', e => {
    const r = e.target.closest('.row'); if (!r) return
    e.preventDefault(); menu(e.clientX, e.clientY, r.dataset.path)
  })
}

// ---- search
function viewSearch(v) {
  v.append(h(`<div class="searchbar ${state.query ? 'has-q' : ''}">${ICON.search}
      <input id="q" spellcheck="false" autocomplete="off" placeholder="${state.snapshot?.config?.content_search === false ? T.search_ph : T.search_ph2}" value="${esc(state.query)}" />
      <span class="meta" id="meta"></span><button class="clear" id="clear">${ICON.x}</button></div>`))
  const chips = h('<div class="chips" id="chips"></div>'); v.append(chips)
  v.append(h(`<div class="toolbar"><span id="count"></span><div class="seg" id="seg"><div class="seg-pill"></div>${
    Object.entries(T.sort).map(([k, l]) => `<button data-s="${k}">${l}</button>`).join('')}</div></div>`))
  const res = h('<div class="results" id="results"><div class="sel" id="sel"></div><div id="rows"></div></div>'); v.append(res)
  v.append(h(`<div class="hint"><span><kbd>↵</kbd>${T.hint_open}</span><span><kbd>⌘↵</kbd>${T.hint_reveal}</span><span><kbd>Space</kbd>${T.hint_ql}</span><span><kbd>⌘C</kbd>${T.hint_copy}</span></div>`))

  const q = $('#q'); q.focus(); q.setSelectionRange(q.value.length, q.value.length)
  let deb
  q.addEventListener('input', () => {
    state.query = q.value; $('.searchbar').classList.toggle('has-q', !!q.value)
    clearTimeout(deb); deb = setTimeout(() => runSearch(true), 60)
  })
  $('#clear').onclick = () => { q.value = ''; state.query = ''; $('.searchbar').classList.remove('has-q'); runSearch(true); q.focus() }
  $('#seg').addEventListener('click', e => { const b = e.target.closest('button'); if (!b) return; state.sort = b.dataset.s; moveSeg(); runSearch(true) })
  res.addEventListener('scroll', () => { if (state.more && res.scrollTop + res.clientHeight > res.scrollHeight - 300) loadMore() })
  bindRows(res)
  renderChips(); moveSeg(); runSearch(true)
}
function moveSeg() {
  const seg = $('#seg'); if (!seg) return
  seg.querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.s === state.sort))
  const on = $(`button[data-s="${state.sort}"]`, seg), pill = $('.seg-pill', seg)
  pill.style.left = on.offsetLeft + 'px'; pill.style.width = on.offsetWidth + 'px'
}
function renderChips() {
  const el = $('#chips'); if (!el) return
  if (!el.children.length) {
    const all = h(`<button class="chip" data-k=""><i style="background:var(--text-2);box-shadow:none"></i>${T.all}<b></b></button>`)
    el.append(all)
    for (const k of KINDS) el.append(h(`<button class="chip" data-k="${k}" style="--c:${kc(k)}"><i></i>${T.kind[k]}<b data-v="0">0</b></button>`))
    el.addEventListener('click', e => {
      const b = e.target.closest('.chip'); if (!b) return
      const k = b.dataset.k
      if (!k) state.kinds.clear()
      else if (e.metaKey || e.shiftKey) state.kinds.has(k) ? state.kinds.delete(k) : state.kinds.add(k)
      else state.kinds = state.kinds.has(k) && state.kinds.size === 1 ? new Set() : new Set([k])
      renderChips(); runSearch(true)
    })
  }
  let sum = 0
  el.querySelectorAll('.chip').forEach(c => {
    const k = c.dataset.k
    c.classList.toggle('on', k ? state.kinds.has(k) : state.kinds.size === 0)
    if (k) { const n = state.counts[k] || 0; sum += n; tween($('b', c), n, { dur: 500 }); c.classList.toggle('zero', n === 0) }
  })
  tween($('.chip[data-k=""] b', el), sum, { dur: 500 })
}
async function runSearch(reset) {
  const seq = ++state.seq
  if (reset) { state.offset = 0; state.sel = 0 }
  const res = await api.search({ query: state.query, kinds: [...state.kinds], sort: state.sort, limit: 80, offset: state.offset })
  if (seq !== state.seq || state.view !== 'search') return
  state.counts = res.kind_counts; state.total = res.total; state.took = res.took_us
  state.hits = reset ? res.hits : state.hits.concat(res.hits)
  state.more = state.hits.length < res.total
  renderChips(); renderRows(reset ? 0 : state.hits.length - res.hits.length)
}
async function loadMore() { if (state.loading) return; state.loading = true; state.offset = state.hits.length; await runSearch(false); state.loading = false }

function renderRows(from) {
  const rows = $('#rows'), terms = state.query.trim().split(/\s+/)
  $('#meta').textContent = state.query || state.kinds.size ? T.matches(fmt(state.total), (state.took / 1000).toFixed(1)) : ''
  $('#count').textContent = T.files(fmt(state.total))
  if (from === 0) rows.innerHTML = ''
  if (!state.hits.length) {
    rows.innerHTML = state.query || state.kinds.size
      ? `<div class="empty"><div><div class="orb" style="opacity:.4"></div><div class="big">${T.no_results}</div><div>${T.no_results_sub}</div></div></div>`
      : emptyHTML()
    rows.querySelectorAll('.tips button').forEach(b => b.onclick = () => { const q = $('#q'); q.value = b.textContent; q.dispatchEvent(new Event('input')) })
    $('#sel').style.opacity = 0; return
  }
  rows.insertAdjacentHTML('beforeend', state.hits.slice(from).map((f, i) => rowHTML(f, terms, from + i)).join(''))
  moveSel(false)
}
function emptyHTML() {
  const tips = ['合同', T.tip_content, 'kind:pdf 发票', 'in:Downloads >100mb', 'after:2025 视频', 'nzzj']
  return `<div class="empty"><div><div class="orb"></div><div class="big">${T.empty_title}</div><div>${T.empty_sub}</div>
    <div class="tips">${tips.map(t => `<button>${esc(t)}</button>`).join('')}</div></div></div>`
}
function moveSel(scroll = true) {
  const sel = $('#sel'), row = $(`#rows .row[data-i="${state.sel}"]`)
  if (!sel || !row) return
  sel.style.opacity = 1; sel.style.transform = `translateY(${row.offsetTop}px)`
  if (scroll) row.scrollIntoView({ block: 'nearest' })
}
const selected = () => state.hits[state.sel]

// ---- simple lists
async function viewList(kind) {
  const v = $('#view')
  const [title, sub, fn] = { recent: [T.recent_title, T.recent_sub, api.recent], large: [T.large_title, T.large_sub, api.largest], forgotten: [T.forgot_title, T.forgot_sub, api.forgotten] }[kind]
  v.append(h(`<div class="head"><div><h1>${title}</h1><p>${sub}</p></div></div>`))
  const sc = h('<div class="scroll"><div class="list" id="list"></div></div>'); v.append(sc)
  bindRows(sc)
  const items = await fn(60)
  $('#list', sc).innerHTML = items.map((f, i) => rowHTML(f, [], i)).join('') || `<div class="empty" style="padding:40px">—</div>`
}

// ---- smart collections
async function viewCollections(v) {
  if (state.collection) return viewCollection(v, state.collection)
  v.append(h(`<div class="head"><div><h1>${T.col_title}</h1><p>${T.col_sub}</p></div></div>`))
  const sc = h('<div class="scroll"></div>'); v.append(sc)
  const stats = await api.collections(), by = Object.fromEntries(stats.map(s => [s.id, s]))
  const grid = h('<div class="grid cols"></div>'); sc.append(grid)
  COLLECTIONS.forEach(([id, color], i) => {
    const s = by[id] || { files: 0, bytes: 0, latest: 0 }
    const c = h(`<button class="card col-card ${s.files ? '' : 'empty'}" style="--c:${color};animation-delay:${i * 50}ms"><div class="glow"></div>
      <div class="col-icon">${ICON.c[id]}</div>
      <div class="col-name">${T.col[id][0]}</div><div class="col-desc">${T.col[id][1]}</div>
      <div class="col-foot"><span class="n" data-v="0">0</span><span>${s.files ? `${bytes(s.bytes)} · ${ago(s.latest)}` : T.col_none}</span></div></button>`)
    c.onclick = () => { state.collection = id; render() }
    grid.append(c); setTimeout(() => tween($('.n', c), s.files, { dur: 900 }), 100 + i * 50)
  })
  sc.append(h(`<p class="col-note">${T.col_note}</p>`))
}
async function viewCollection(v, id) {
  const color = Object.fromEntries(COLLECTIONS)[id]
  const head = h(`<div class="head"><div class="col-head" style="--c:${color}"><button class="back">${ICON.back}</button><div class="col-icon sm">${ICON.c[id]}</div>
    <div><h1>${T.col[id][0]}</h1><p id="col-count"></p></div></div></div>`)
  $('.back', head).onclick = () => { state.collection = null; render() }
  v.append(head)
  const sc = h('<div class="scroll"><div class="list" id="list"></div></div>'); v.append(sc)
  bindRows(sc)
  const items = await api.collection(id, 300)
  $('#col-count').textContent = T.files(fmt(items.length))
  $('#list', sc).innerHTML = items.map((f, i) => rowHTML(f, [], i)).join('') || `<div class="empty" style="padding:40px">${T.col_none}</div>`
}

// ---- timeline
async function viewTimeline(v) {
  if (state.month) return viewMonth(v, state.month)
  v.append(h(`<div class="head"><div><h1>${T.tl_title}</h1><p>${T.tl_sub}</p></div></div>`))
  const sc = h('<div class="scroll"></div>'); v.append(sc)
  const months = await api.timeline()
  if (!months.length) { sc.append(h(`<div class="empty" style="padding:60px">${T.tl_empty}</div>`)); return }
  const max = Math.max(...months.map(m => m.files), 1)
  const tl = h('<div class="tl"></div>'); sc.append(tl)
  let year = null, i = 0
  for (const m of months) {
    const y = m.month.slice(0, 4)
    if (y !== year) { year = y; tl.append(h(`<div class="tl-year"><span>${T.tl_year(y)}</span><b>${T.files(fmt(months.filter(x => x.month.startsWith(y)).reduce((a, x) => a + x.files, 0)))}</b></div>`)) }
    const total = m.kinds.reduce((a, k) => a + k.files, 0) || 1
    const r = h(`<button class="tl-row" style="animation-delay:${Math.min(i++, 20) * 22}ms"><i class="dot"></i>
      <div class="tl-m">${T.tl_month(m.month)}</div>
      <div class="tl-bar"><div class="tl-fill" data-w="${(Math.max(m.files / max, 0.02) * 100).toFixed(1)}">${m.kinds.map(k => `<b style="--c:${kc(k.kind)};flex:${k.files / total}"></b>`).join('')}</div></div>
      <div class="tl-kinds">${m.kinds.slice(0, 3).map(k => `<span style="--c:${kc(k.kind)}"><i></i>${T.kind[k.kind]} ${fmt(k.files)}</span>`).join('')}</div>
      <div class="tl-n">${fmt(m.files)}</div><div class="tl-s">${bytes(m.bytes)}</div></button>`)
    r.onclick = () => { state.month = m.month; state.monthKind = null; render() }
    tl.append(r)
  }
  requestAnimationFrame(() => requestAnimationFrame(() => sc.querySelectorAll('[data-w]').forEach(b => b.style.width = b.dataset.w + '%')))
}
async function viewMonth(v, month) {
  const head = h(`<div class="head"><div class="col-head" style="--c:var(--accent)"><button class="back">${ICON.back}</button><div class="col-icon sm">${ICON.timeline}</div>
    <div><h1>${T.tl_month_full(month)}</h1><p id="col-count"></p></div></div></div>`)
  $('.back', head).onclick = () => { state.month = null; render() }
  v.append(head)
  const chips = h('<div class="chips tl-chips"></div>'); v.append(chips)
  const sc = h('<div class="scroll"><div class="list" id="list"></div></div>'); v.append(sc)
  bindRows(sc)
  const all = await api.month(month, null, 1000)
  const counts = {}; all.forEach(f => counts[f.kind] = (counts[f.kind] || 0) + 1)
  const draw = () => {
    const items = state.monthKind ? all.filter(f => f.kind === state.monthKind) : all
    $('#col-count').textContent = T.files(fmt(items.length))
    $('#list', sc).innerHTML = items.slice(0, 400).map((f, i) => rowHTML(f, [], i)).join('') || `<div class="empty" style="padding:40px">—</div>`
    chips.innerHTML = [`<button class="chip ${state.monthKind ? '' : 'on'}" data-k="">${T.all} <b>${fmt(all.length)}</b></button>`,
      ...KINDS.filter(k => counts[k]).map(k => `<button class="chip ${state.monthKind === k ? 'on' : ''}" data-k="${k}" style="--c:${kc(k)}"><i></i>${T.kind[k]} <b>${fmt(counts[k])}</b></button>`)].join('')
    chips.querySelectorAll('button').forEach(b => b.onclick = () => { state.monthKind = b.dataset.k || null; draw() })
  }
  draw()
}

// ---- share card: a picture of "your Mac in files" for WeChat / Xiaohongshu
async function shareCard(ov, totalFiles, totalBytes) {
  const W = 1080, H = 1440, c = document.createElement('canvas'); c.width = W; c.height = H
  const g = c.getContext('2d'), css = getComputedStyle(document.documentElement)
  const color = k => css.getPropertyValue(`--k-${k}`).trim() || '#8b7cf6'
  const bg = g.createLinearGradient(0, 0, W, H); bg.addColorStop(0, '#1b1640'); bg.addColorStop(0.55, '#0f1424'); bg.addColorStop(1, '#2a1426')
  g.fillStyle = bg; g.fillRect(0, 0, W, H)
  const glow = (x, y, r, col) => { const gr = g.createRadialGradient(x, y, 0, x, y, r); gr.addColorStop(0, col); gr.addColorStop(1, 'transparent'); g.fillStyle = gr; g.fillRect(0, 0, W, H) }
  glow(200, 160, 620, 'rgba(139,124,246,.45)'); glow(980, 1300, 700, 'rgba(244,114,182,.22)'); glow(900, 420, 420, 'rgba(56,189,248,.16)')
  const font = (w, px) => `${w} ${px}px -apple-system, "PingFang SC", "PingFang TC", "Helvetica Neue", sans-serif`
  g.fillStyle = 'rgba(255,255,255,.65)'; g.font = font(600, 40); g.fillText(T.share_title, 90, 150)
  g.fillStyle = '#fff'; g.font = font(800, 200); g.fillText(fmt(totalFiles), 84, 380)
  g.fillStyle = 'rgba(255,255,255,.8)'; g.font = font(600, 46)
  g.fillText(`${T.share_files} · ${bytes(totalBytes)}`, 90, 460)
  // stacked bar by kind
  let x = 90; const bw = W - 180, top = 540
  g.save(); g.beginPath(); g.roundRect(90, top, bw, 34, 17); g.clip()
  for (const k of ov.kinds) { const w = bw * k.bytes / (totalBytes || 1); g.fillStyle = color(k.kind); g.fillRect(x, top, w + 1, 34); x += w }
  g.restore()
  // legend: top kinds with counts
  ov.kinds.slice(0, 6).forEach((k, i) => {
    const cx = 90 + (i % 2) * 460, cy = 680 + Math.floor(i / 2) * 92
    g.fillStyle = color(k.kind); g.beginPath(); g.arc(cx + 14, cy - 14, 14, 0, 7); g.fill()
    g.fillStyle = '#fff'; g.font = font(700, 42); g.fillText(T.kind[k.kind], cx + 46, cy)
    g.fillStyle = 'rgba(255,255,255,.55)'; g.font = font(500, 34); g.fillText(fmt(k.files), cx + 46 + g.measureText(T.kind[k.kind]).width * 1.25 + 16, cy)
  })
  // facts
  const facts = []
  const sc = [...ov.kinds].sort((a, b) => b.folders - a.folders)[0]
  if (sc) facts.push(T.share_scatter(T.kind[sc.kind], fmt(sc.folders)))
  if (ov.forgotten_files) facts.push(T.share_forgot(fmt(ov.forgotten_files)) + ' · ' + bytes(ov.forgotten_bytes))
  if (ov.changed_7d) facts.push(T.share_week(fmt(ov.changed_7d)))
  facts.forEach((f, i) => {
    const y = 1010 + i * 110
    g.fillStyle = 'rgba(255,255,255,.08)'; g.beginPath(); g.roundRect(90, y - 66, W - 180, 90, 24); g.fill()
    g.fillStyle = '#fff'; g.font = font(600, 38); g.fillText(f, 130, y - 8)
  })
  g.fillStyle = 'rgba(255,255,255,.5)'; g.font = font(500, 32); g.fillText(T.share_foot, 90, H - 90)
  g.fillStyle = 'rgba(255,255,255,.85)'; g.font = font(800, 40); const lw = g.measureText('Lumen').width; g.fillText('Lumen', W - 90 - lw, H - 90)
  return c
}
async function openShare(ov, totalFiles, totalBytes) {
  const canvas = await shareCard(ov, totalFiles, totalBytes)
  const m = h(`<div class="modal-bg"><div class="modal share"><img alt=""><div class="modal-actions">
    <button class="btn" data-a="close">${T.share_close}</button><button class="btn primary" data-a="save">${ICON.share}${T.share_save}</button></div></div></div>`)
  $('img', m).src = canvas.toDataURL('image/png')
  const close = () => { m.classList.add('out'); setTimeout(() => m.remove(), 180) }
  m.onclick = e => { if (e.target === m || e.target.dataset.a === 'close') close() }
  $('[data-a="save"]', m).onclick = async () => {
    const blob = await new Promise(r => canvas.toBlob(r, 'image/png'))
    try { await api.saveShareCard(new Uint8Array(await blob.arrayBuffer())); toast(T.share_saved); close() } catch (e) { toast(String(e)) }
  }
  document.body.append(m)
}

// ---- insights
async function viewInsights(v) {
  const head = h(`<div class="head"><div><h1>${T.ins_title}</h1><p>${T.ins_sub}</p></div><button class="btn" id="share">${ICON.share}${T.share_btn}</button></div>`)
  v.append(head)
  const sc = h('<div class="scroll"></div>'); v.append(sc)
  const ov = await api.overview()
  $('#share', head).onclick = () => openShare(ov, ov.kinds.reduce((a, k) => a + k.files, 0), ov.kinds.reduce((a, k) => a + k.bytes, 0))
  const totalFiles = ov.kinds.reduce((a, k) => a + k.files, 0), totalBytes = ov.kinds.reduce((a, k) => a + k.bytes, 0)
  const scattered = [...ov.kinds].sort((a, b) => b.folders - a.folders)[0]
  const hero = h(`<div class="card hero-card" style="--c:var(--accent)"><div class="glow"></div>
    <div><h2>${T.ins_total}</h2><div class="n" id="ins-n">0</div><div class="s">${bytes(totalBytes)}</div>
      <div class="stack">${ov.kinds.map(k => `<b style="--c:${kc(k.kind)}" data-w="${(k.bytes / totalBytes * 100).toFixed(2)}"></b>`).join('')}</div>
      <div class="legend">${ov.kinds.slice(0, 6).map(k => `<span style="--c:${kc(k.kind)}"><i></i>${T.kind[k.kind]} ${bytes(k.bytes)}</span>`).join('')}</div></div>
    <div class="insight-line">
      ${scattered ? `<p>${T.ins_scatter(T.kind[scattered.kind], fmt(scattered.folders))}</p>` : ''}
      <p>${T.ins_week(fmt(ov.changed_7d))}</p>
      ${ov.forgotten_files ? `<p>${T.ins_forgot(fmt(ov.forgotten_files), bytes(ov.forgotten_bytes))}</p>` : ''}
    </div></div>`)
  const grid = h('<div class="grid"></div>'); grid.append(hero); sc.append(grid)
  sc.append(h(`<div class="section-title">${T.ins_kinds}</div>`))
  const g2 = h('<div class="grid"></div>'); sc.append(g2)
  const maxB = Math.max(...ov.kinds.map(k => k.bytes), 1)
  ov.kinds.forEach((k, i) => {
    const c = h(`<div class="card" style="--c:${kc(k.kind)};animation-delay:${i * 50}ms"><div class="glow"></div>
      <div class="k"><i></i>${T.kind[k.kind]}</div><div class="n" data-v="0">0</div>
      <div class="s">${bytes(k.bytes)} · ${T.in_folders(fmt(k.folders))}</div><div class="bar"><b data-w="${(k.bytes / maxB * 100).toFixed(1)}"></b></div></div>`)
    g2.append(c); setTimeout(() => tween($('.n', c), k.files, { dur: 1100 }), 120 + i * 50)
  })
  sc.append(h(`<div class="section-title">${T.ins_folders}</div>`))
  const list = h('<div class="list"></div>'); sc.append(list)
  ov.top_folders.forEach((f, i) => {
    const r = h(`<div class="row" data-path="${esc(f.path)}" style="grid-template-columns:36px 1fr auto auto;animation-delay:${i * 40}ms">
      <div class="tile">${ICON.folder}</div><div class="txt"><div class="name">${esc(f.path.split('/').pop() || f.path)}</div><div class="path"><span>${esc(shortPath(f.path))}</span></div></div>
      <div class="size">${T.files(fmt(f.files))}</div><div class="date">${bytes(f.bytes)}</div></div>`)
    r.ondblclick = () => act('reveal', f.path)
    list.append(r)
  })
  tween($('#ins-n'), totalFiles, { dur: 1300 })
  requestAnimationFrame(() => requestAnimationFrame(() => sc.querySelectorAll('[data-w]').forEach(b => b.style.width = b.dataset.w + '%')))
}

// ---- duplicates
async function viewDupes(v) {
  v.append(h(`<div class="head"><div><h1>${T.dupes_title}</h1><p>${T.dupes_sub}</p></div><div class="s" id="dupe-total"></div></div>`))
  const sc = h(`<div class="scroll"><div class="empty" style="padding:60px"><div><div class="orb"></div><div class="big">${T.dupes_loading}</div></div></div></div>`); v.append(sc)
  bindRows(sc)
  const groups = await api.duplicates()
  if (state.view !== 'dupes') return
  if (!groups.length) { sc.innerHTML = `<div class="empty" style="padding:60px"><div><div class="big">${T.dupes_none}</div></div></div>`; return }
  let wasted = groups.reduce((a, g) => a + g.wasted, 0)
  const keep = groups.map(() => 0)
  const head = $('#dupe-total')
  const drawHead = () => {
    head.innerHTML = wasted > 0 ? `<button class="btn danger" id="dupe-all">${ICON.trash}${T.dupes_trash_all(bytes(wasted))}</button>` : ''
    const b = $('#dupe-all'); if (b) armed(b, T.dupes_all_confirm, async () => {
      const paths = [], live = []
      groups.forEach((g, gi) => { if (g.done) return; live.push(gi); g.files.forEach((f, i) => i !== keep[gi] && paths.push(f.path)) })
      if (await trash(paths)) live.forEach(gi => settle(gi))
    })
  }
  sc.innerHTML = groups.map((g, gi) => `<div class="list dupe" data-g="${gi}" style="animation:rowIn .5s var(--ease) ${gi * 60}ms both">
    <div class="dupe-head"><span>${esc(g.files[0].name)} · ${T.dupes_copies(g.files.length)}</span>
      <span class="dupe-acts"><b>${T.dupes_wasted(bytes(g.wasted))}</b><button class="btn small danger" data-trash="${gi}">${ICON.trash}${T.dupes_trash(g.files.length - 1)}</button></span></div>
    ${g.files.map((f, i) => rowHTML(f, [], i, `<button class="keep ${i === 0 ? 'on' : ''}" data-keep="${i}" title="${T.dupes_kept}">${ICON.check}<span>${T.dupes_keep}</span></button>`)).join('')}</div>`).join('')
  sc.querySelectorAll('.dupe').forEach(el => {
    const gi = Number(el.dataset.g)
    el.addEventListener('click', e => {
      const k = e.target.closest('[data-keep]'); if (!k) return
      e.stopPropagation(); keep[gi] = Number(k.dataset.keep)
      el.querySelectorAll('[data-keep]').forEach(b => b.classList.toggle('on', b === k))
      el.querySelectorAll('.row').forEach((r, i) => r.classList.toggle('kept', i === keep[gi]))
    })
    el.querySelectorAll('.row')[0]?.classList.add('kept')
    armed($('[data-trash]', el), T.dupes_confirm, async () => {
      const g = groups[gi]
      if (await trash(g.files.filter((_, i) => i !== keep[gi]).map(f => f.path))) settle(gi)
    })
  })
  drawHead()
  function settle(gi) {
    const g = groups[gi]; if (g.done) return
    g.done = true; wasted -= g.wasted
    const el = $(`.dupe[data-g="${gi}"]`, sc)
    el.querySelectorAll('.row:not(.kept)').forEach(r => r.classList.add('gone'))
    $('.dupe-acts', el).innerHTML = `<b class="ok">${ICON.check}${T.dupes_kept}</b>`
    setTimeout(() => el.classList.add('done'), 700)
    drawHead()
  }
}

/** A destructive button that needs a second click within 3 s. */
function armed(btn, confirmText, run) {
  const label = btn.innerHTML; let t
  btn.onclick = async e => {
    e.stopPropagation()
    if (!btn.classList.contains('armed')) {
      btn.classList.add('armed'); btn.innerHTML = `${ICON.trash}${confirmText}`
      t = setTimeout(() => { btn.classList.remove('armed'); btn.innerHTML = label }, 3000); return
    }
    clearTimeout(t); btn.disabled = true; await run()
  }
}
async function trash(paths) {
  if (!paths.length) return true
  try { const r = await api.trash(paths); toast(T.trashed(r.count, bytes(r.bytes))); refreshSnapshot(); return true }
  catch { toast(T.trash_fail); return false }
}

// ---- settings
async function viewSettings(v) {
  const snap = await api.snapshot(); state.snapshot = snap
  v.append(h(`<div class="head"><div><h1>${T.settings_title}</h1><p>${T.settings_sub}</p></div></div>`))
  const sc = h('<div class="scroll"><div class="form"></div></div>'); v.append(sc)
  const form = $('.form', sc)
  const pathList = (title, p, items, addLabel, save) => {
    const f = h(`<div class="field"><h3>${title}</h3><p>${p}</p><div class="paths"></div><button class="btn">${ICON.plus}${addLabel}</button></div>`)
    const draw = () => {
      $('.paths', f).innerHTML = items.map((x, i) => `<div class="pathrow">${ICON.folder}<span>${esc(shortPath(x))}</span><button data-i="${i}">${ICON.x}</button></div>`).join('')
      $('.paths', f).querySelectorAll('button').forEach(b => b.onclick = () => { items.splice(Number(b.dataset.i), 1); draw(); save(items) })
    }
    $('.btn', f).onclick = async () => { const p = await api.pickFolder(); if (p && !items.includes(p)) { items.push(p); draw(); save(items) } }
    draw(); return f
  }
  form.append(pathList(T.set_roots, T.set_roots_p, [...snap.config.roots], T.set_add, items => api.setRoots(items).then(scanStarted)))
  form.append(pathList(T.set_excl, T.set_excl_p, [...snap.config.excluded_paths], T.set_add_excl, items => api.setExcluded(items).then(scanStarted)))
  if (!snap.fullDiskAccess) form.prepend(fdaCard())
  const feats = h(`<div class="field"><h3>${T.set_features}</h3>
    ${toggleRow('content', T.set_content, T.set_content_p, snap.config.content_search !== false)}
    ${toggleRow('ocr', T.set_ocr, T.set_ocr_p, snap.config.ocr !== false)}
    ${toggleRow('external', T.set_external, T.set_external_p, snap.config.include_external !== false)}</div>`)
  form.append(feats)
  const gen = h(`<div class="field"><h3>${T.set_general}</h3>
    ${toggleRow('quick', T.set_quick, T.set_quick_p, snap.config.quick_panel !== false)}
    ${toggleRow('login', T.set_login, T.set_login_p, !!snap.config.launch_at_login)}
    <div class="toggle"><div><b>${T.set_theme}</b></div><div class="seg plain" id="theme">${['auto', 'light', 'dark'].map(t =>
      `<button data-t="${t}" class="${(snap.config.theme || 'auto') === t ? 'on' : ''}">${T['theme_' + t]}</button>`).join('')}</div></div></div>`)
  form.insertBefore(gen, form.querySelector('.field:not(.fda)'))
  gen.querySelectorAll('.switch').forEach(sw => sw.onclick = async () => {
    const on = !sw.classList.contains('on'); sw.classList.toggle('on', on)
    const cfg = await api.setOptions(sw.dataset.k === 'quick' ? { quickPanel: on } : { launchAtLogin: on })
    state.snapshot.config = { ...state.snapshot.config, ...cfg }
  })
  gen.querySelectorAll('#theme button').forEach(b => b.onclick = async () => {
    gen.querySelectorAll('#theme button').forEach(x => x.classList.toggle('on', x === b))
    applyTheme(b.dataset.t)
    const cfg = await api.setOptions({ theme: b.dataset.t }); state.snapshot.config = { ...state.snapshot.config, ...cfg }
  })
  feats.querySelectorAll('.switch').forEach(sw => sw.onclick = async () => {
    const on = !sw.classList.contains('on'); sw.classList.toggle('on', on)
    const cfg = await api.setOptions({ content: { contentSearch: on }, ocr: { ocr: on }, external: { includeExternal: on } }[sw.dataset.k])
    state.snapshot.config = { ...state.snapshot.config, ...cfg }; scanStarted()
  })
  const hk = h(`<div class="field"><h3>${T.set_hotkey_title}</h3><p>${T.set_shortcuts_p}</p>
    <button class="hotkey" id="hk">${keycaps(snap.config.hotkey || 'Alt+Space')}</button></div>`)
  form.append(hk)
  recordHotkey($('#hk', hk))
  const upd = h(`<div class="field"><h3>${T.set_update}</h3>
    ${toggleRow('update', T.set_update, T.set_update_p(snap.version || ''), snap.config.auto_update !== false)}
    <button class="btn" id="upd-check">${T.set_update_check}</button></div>`)
  $('h3', upd).remove()
  form.append(upd)
  $('.switch', upd).onclick = async e => {
    const sw = e.currentTarget, on = !sw.classList.contains('on'); sw.classList.toggle('on', on)
    const cfg = await api.setOptions({ autoUpdate: on }); state.snapshot.config = { ...state.snapshot.config, ...cfg }
  }
  $('#upd-check', upd).onclick = async () => {
    try { const u = await api.checkUpdate(); u ? updateBanner(u) : toast(T.upd_none) } catch { toast(T.upd_fail) }
  }
  form.append(h(`<div class="field"><h3>${T.set_privacy}</h3><p>${T.set_privacy_p}</p><button class="btn primary" id="set-rescan">${T.set_rescan}</button></div>`))
  $('#set-rescan').onclick = () => api.startScan().then(scanStarted)
}

function toggleRow(k, title, p, on) {
  return `<div class="toggle"><div><b>${title}</b><p>${p}</p></div><button class="switch ${on ? 'on' : ''}" data-k="${k}"><i></i></button></div>`
}
function fdaCard() {
  const c = h(`<div class="field fda"><div class="fda-icon">${ICON.shield}</div><div><h3>${T.fda_title}</h3><p>${T.fda_p}</p>
    <button class="btn primary">${T.fda_btn}</button></div></div>`)
  $('.btn', c).onclick = () => api.openFdaSettings()
  return c
}
const KEYNAMES = { Alt: '⌥', Option: '⌥', Cmd: '⌘', Command: '⌘', CmdOrCtrl: '⌘', Super: '⌘', Ctrl: '⌃', Control: '⌃', Shift: '⇧', Space: 'Space' }
const keycaps = acc => acc.split('+').map(k => `<kbd>${KEYNAMES[k] || k.replace(/^Key/, '')}</kbd>`).join('')
function recordHotkey(btn) {
  btn.onclick = () => {
    btn.classList.add('rec'); btn.innerHTML = `<span>${T.set_hotkey_rec}</span>`
    const onKey = async e => {
      e.preventDefault(); e.stopPropagation()
      if (e.key === 'Escape') return done(state.snapshot.config.hotkey)
      if (['Meta', 'Alt', 'Control', 'Shift'].includes(e.key)) return
      const mods = [e.metaKey && 'Cmd', e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift'].filter(Boolean)
      if (!mods.length) return
      const code = e.code.replace(/^Key/, '').replace(/^Digit/, '')
      const acc = [...mods, code].join('+')
      try { const cfg = await api.setHotkey(acc); state.snapshot.config = { ...state.snapshot.config, ...cfg }; toast(T.set_hotkey_ok(acc.replace(/\+/g, ' '))); done(acc) }
      catch { toast(T.set_hotkey_taken); btn.classList.add('shake'); setTimeout(() => btn.classList.remove('shake'), 500); done(state.snapshot.config.hotkey) }
    }
    const done = acc => { removeEventListener('keydown', onKey, true); btn.classList.remove('rec'); btn.innerHTML = keycaps(acc || 'Alt+Space') }
    addEventListener('keydown', onKey, true)
  }
}

// ------------------------------------------------------------ updates ----
function updateBanner(u) {
  if ($('.upd-banner')) return
  const b = h(`<div class="upd-banner"><div class="upd-icon">${ICON.spark}</div><div class="upd-txt"><b>${T.upd_avail(esc(u.version))}</b><span></span></div>
    <button class="btn" data-a="later">${T.upd_later}</button><button class="btn primary" data-a="go">${T.upd_install}</button></div>`)
  $('[data-a="later"]', b).onclick = () => { b.classList.add('out'); setTimeout(() => b.remove(), 200) }
  $('[data-a="go"]', b).onclick = async e => {
    e.currentTarget.disabled = true; $('[data-a="later"]', b).remove()
    api.on('update-progress', p => { $('.upd-txt span', b).textContent = T.upd_progress(Math.round(p * 100)) })
    try { await api.installUpdate() } catch { toast(T.upd_fail); b.remove() }
  }
  document.body.append(b)
}

// ------------------------------------------------------------ actions ----
async function act(what, path) {
  try {
    if (what === 'open') await api.open(path)
    else if (what === 'reveal') await api.reveal(path)
    else if (what === 'ql') await api.quickLook(path)
    else if (what === 'copy') { await navigator.clipboard.writeText(path); toast(T.copied) }
    else if (what === 'copyname') { await navigator.clipboard.writeText(path.split('/').pop()); toast(T.copied) }
  } catch (e) {
    if (String(e).includes('moved')) { toast(T.moved); if (state.view === 'search') runSearch(true) }
  }
}
function menu(x, y, path) {
  closeMenu()
  const m = h(`<div class="menu" id="menu">
    <button data-a="open">${T.menu_open}<kbd>↵</kbd></button><button data-a="reveal">${T.menu_reveal}<kbd>⌘↵</kbd></button>
    <button data-a="ql">${T.menu_ql}<kbd>Space</kbd></button><button data-a="copy">${T.menu_copy}<kbd>⌘C</kbd></button>
    <button data-a="copyname">${T.menu_copy_name}</button></div>`)
  document.body.append(m)
  const r = m.getBoundingClientRect()
  m.style.left = Math.min(x, innerWidth - r.width - 8) + 'px'; m.style.top = Math.min(y, innerHeight - r.height - 8) + 'px'
  m.onclick = e => { const b = e.target.closest('button'); if (b) { act(b.dataset.a, path); closeMenu() } }
  setTimeout(() => addEventListener('mousedown', closeMenuOutside), 0)
}
function closeMenuOutside(e) { if (!e.target.closest('#menu')) closeMenu() }
function closeMenu() { $('#menu')?.remove(); removeEventListener('mousedown', closeMenuOutside) }

function toast(text) {
  if (!text) return
  const t = h(`<div class="toast"><i></i>${esc(text)}</div>`); $('#toasts').append(t)
  setTimeout(() => { t.classList.add('out'); setTimeout(() => t.remove(), 400) }, 2600)
}

// ------------------------------------------------------------ keyboard ---
addEventListener('keydown', e => {
  if ($('#menu') && e.key === 'Escape') return closeMenu()
  if (!$('#stage').hidden) return
  if (e.metaKey && /^[1-9]$/.test(e.key)) { e.preventDefault(); go(NAV.filter(x => x !== 'sep')[Number(e.key) - 1][0]); return }
  if (e.metaKey && e.key.toLowerCase() === 'k') { e.preventDefault(); if (state.view !== 'search') go('search'); else $('#q')?.select(); return }
  if (state.view !== 'search') return
  const q = $('#q'), f = selected()
  if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) { e.preventDefault(); state.sel = Math.min(state.hits.length - 1, state.sel + 1); moveSel(); if (state.sel > state.hits.length - 10 && state.more) loadMore() }
  else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) { e.preventDefault(); state.sel = Math.max(0, state.sel - 1); moveSel() }
  else if (e.key === 'Enter' && f) { e.preventDefault(); act(e.metaKey ? 'reveal' : 'open', f.path) }
  else if (e.key === ' ' && f && (document.activeElement !== q || !q.value || e.shiftKey || q.selectionStart === 0)) {
    if (document.activeElement !== q || e.shiftKey) { e.preventDefault(); act('ql', f.path) }
  }
  else if (e.metaKey && e.key.toLowerCase() === 'c' && f && !getSelection().toString() && q.selectionStart === q.selectionEnd) { e.preventDefault(); act('copy', f.path) }
  else if (e.key === 'Escape') { if (q.value) { q.value = ''; q.dispatchEvent(new Event('input')) } else if (isApp) window.__TAURI__.window.getCurrentWindow().hide() }
  else if (document.activeElement !== q && e.key.length === 1 && !e.metaKey && !e.ctrlKey) q.focus()
})

// ------------------------------------------------------------ live status
function setLive(snap) {
  const live = $('#live')
  live.classList.toggle('scanning', state.scanning)
  $('#rescan').classList.toggle('spin', state.scanning)
  tween($('#live-count'), snap?.status?.files ?? 0, { format: n => T.files(fmt(n)) })
  $('#live-sub').textContent = state.scanning ? T.live_scanning
    : state.reading ? T.live_reading(fmt(state.reading.done), fmt(state.reading.total))
    : snap?.status?.last_scan_at ? `${T.live_watching} · ${ago(snap.status.last_scan_at)}` : T.live_watching
}
async function refreshSnapshot() { const s = await api.snapshot(); state.snapshot = s; state.home = s.home; state.scanning = s.scanning; if (!s.reading) state.reading = null; setLive(s); return s }
function scanStarted() { state.scanning = true; setLive(state.snapshot) }

// ------------------------------------------------------------ stage ------
function showStage(el) {
  const st = $('#stage'); st.innerHTML = ''; st.append(el); st.hidden = false; $('#app').classList.add('dimmed')
}
function hideStage() { $('#stage').hidden = true; $('#app').classList.remove('dimmed'); render() }

function onboarding() {
  const yes = T.ob_scope_yes.map((s, i) => `<div style="--c:${kc(['pdf', 'video', 'image'][i])};animation-delay:${200 + i * 70}ms"><b>✓</b>${s}</div>`).join('')
  const no = T.ob_scope_no.map((s, i) => `<div class="no" style="--c:var(--text-3);animation-delay:${410 + i * 70}ms"><b>×</b>${s}</div>`).join('')
  const el = h(`<div class="wrap"><div class="hero-orb"><div class="ring"></div><div class="ring"></div><div class="core"></div></div>
    <h1>${T.ob_title}</h1><p class="lede">${T.ob_lede}</p>
    <div class="scope">${yes}${no}</div>
    ${state.snapshot && !state.snapshot.fullDiskAccess ? `<button class="fda-pill" id="ob-fda">${ICON.shield}<span>${T.fda_title}</span></button>` : ''}
    <button class="btn primary big" id="ob-go">${T.ob_start}</button>
    <p class="fine">${T.ob_fine} · <a id="ob-choose">${T.ob_choose}</a></p></div>`)
  showStage(el)
  $('#ob-go').onclick = () => startFirstScan()
  const fda = $('#ob-fda'); if (fda) fda.onclick = () => api.openFdaSettings()
  $('#ob-choose').onclick = async () => { const p = await api.pickFolder(); if (p) { await api.setRoots([p]); scanningStage() } }
}
async function startFirstScan() { scanningStage(); await api.startScan() }

let radar
function scanningStage() {
  const el = h(`<div class="wrap"><div class="radar"><canvas width="600" height="600"></canvas></div>
    <div class="counter" id="sc-n">0</div><div class="counter-sub" id="sc-sub">${T.scan_title}</div>
    <div class="ticker" id="sc-t"></div><div class="scan-actions"><button class="btn" id="sc-x">${T.scan_cancel}</button></div></div>`)
  showStage(el)
  $('#sc-x').onclick = () => api.cancelScan()
  radar = new Radar($('canvas', el))
}
function onProgress(p) {
  state.scanning = true
  if ($('#stage').hidden) { $('#live-sub').textContent = `${T.live_scanning} ${fmt(p.files_found)}`; return }
  const n = $('#sc-n'); if (!n) return
  n.textContent = fmt(p.files_found)
  const rate = p.elapsed_ms ? Math.round(p.files_found / (p.elapsed_ms / 1000)) : 0
  $('#sc-sub').textContent = T.scan_sub(fmt(p.dirs), fmt(rate))
  $('#sc-t').innerHTML = `<span style="direction:ltr;unicode-bidi:plaintext">${esc(shortPath(p.current || ''))}</span>`
  radar?.feed(p.files_found)
}
async function onDone(r) {
  state.scanning = false
  const snap = await refreshSnapshot()
  if ($('#stage').hidden || !$('#sc-n')) {
    if (!r.full && (r.added || r.changed || r.removed)) toast(T.toast_scan(r.added, r.changed, r.removed))
    if (state.view === 'search') runSearch(true)
    return
  }
  radar?.stop()
  const ov = await api.overview()
  const folders = ov.kinds.reduce((a, k) => Math.max(a, k.folders), 0)
  const top = ov.kinds.slice(0, 5)
  const el = h(`<div class="wrap"><div class="hero-orb"><div class="ring"></div><div class="ring"></div><div class="core"></div></div>
    <h1>${T.done_title(`<span id="dn">0</span>`)}</h1><p class="lede">${T.done_sub(fmt(r.dirs || folders))}</p>
    <div class="donecards">${top.map((k, i) => `<div style="--c:${kc(k.kind)};animation-delay:${300 + i * 80}ms"><b>${fmt(k.files)}</b><span>${T.kind[k.kind]}</span></div>`).join('')}</div>
    <button class="btn primary big" id="dn-go">${T.done_go}</button><p class="fine">${T.done_hint}</p></div>`)
  showStage(el)
  tween($('#dn'), snap.status.files, { dur: 1400 })
  $('#dn-go').onclick = () => { state.view = 'search'; movePill(); hideStage() }
}

/** Sonar sweep; every batch of found files lights up dots in kind colours. */
class Radar {
  constructor(canvas) {
    this.c = canvas; this.x = canvas.getContext('2d'); this.dots = []; this.last = 0; this.a = 0; this.on = true
    this.colors = KINDS.map(k => getComputedStyle(document.documentElement).getPropertyValue(`--k-${k}`).trim())
    requestAnimationFrame(t => this.frame(t))
  }
  feed(n) {
    const d = n - this.last; this.last = n
    const spawn = Math.min(18, Math.ceil(d / 400))
    for (let i = 0; i < spawn; i++) {
      const r = 40 + Math.random() * 240, a = this.a - Math.random() * 0.6
      this.dots.push({ x: 300 + Math.cos(a) * r, y: 300 + Math.sin(a) * r, life: 1, s: 1.5 + Math.random() * 2.5, c: this.colors[Math.floor(Math.random() * this.colors.length)] })
    }
    if (this.dots.length > 400) this.dots.splice(0, this.dots.length - 400)
  }
  stop() { this.on = false }
  frame(t) {
    if (!this.on || !this.c.isConnected) return
    const x = this.x; x.clearRect(0, 0, 600, 600)
    this.a = (t / 1000) * 1.6
    for (let i = 1; i <= 4; i++) { x.beginPath(); x.arc(300, 300, i * 70, 0, Math.PI * 2); x.strokeStyle = `rgba(255,255,255,${0.04 + i * 0.012})`; x.lineWidth = 1; x.stroke() }
    const g = x.createConicGradient(this.a - Math.PI * 0.5, 300, 300)
    g.addColorStop(0, 'rgba(139,123,255,0)'); g.addColorStop(0.1, 'rgba(139,123,255,0)'); g.addColorStop(0.245, 'rgba(192,139,255,.34)'); g.addColorStop(0.25, 'rgba(255,154,118,.9)'); g.addColorStop(0.251, 'rgba(139,123,255,0)')
    x.save(); x.rotate(0); x.beginPath(); x.arc(300, 300, 290, 0, Math.PI * 2); x.fillStyle = g; x.fill(); x.restore()
    for (const d of this.dots) {
      d.life -= 0.006
      if (d.life <= 0) continue
      x.globalAlpha = Math.max(0, d.life)
      x.shadowBlur = 12; x.shadowColor = d.c
      x.beginPath(); x.arc(d.x, d.y, d.s, 0, Math.PI * 2); x.fillStyle = d.c; x.fill()
    }
    x.globalAlpha = 1; x.shadowBlur = 0
    this.dots = this.dots.filter(d => d.life > 0)
    const core = x.createRadialGradient(300, 300, 0, 300, 300, 46)
    core.addColorStop(0, 'rgba(255,255,255,.95)'); core.addColorStop(.35, 'rgba(199,188,255,.7)'); core.addColorStop(1, 'rgba(139,123,255,0)')
    x.fillStyle = core; x.beginPath(); x.arc(300, 300, 46 + Math.sin(t / 300) * 3, 0, Math.PI * 2); x.fill()
    requestAnimationFrame(tt => this.frame(tt))
  }
}

// ------------------------------------------------------------ boot -------
async function boot() {
  renderNav()
  $('#rescan').title = T.set_rescan
  $('#rescan').onclick = () => api.startScan().then(scanStarted)
  await api.on('scan://progress', onProgress)
  await api.on('scan://done', onDone)
  await api.on('scan://error', e => { state.scanning = false; toast(String(e)) })
  await api.on('index://changed', async b => { toast(T.toast_changed(b.upserted, b.removed)); await refreshSnapshot(); if (state.view === 'search') runSearch(true) })
  await api.on('content://progress', p => {
    state.reading = p.finished ? null : p
    const live = $('#live'); live.classList.toggle('scanning', !!state.reading || state.scanning)
    if (!state.scanning) $('#live-sub').textContent = state.reading ? T.live_reading(fmt(p.done), fmt(p.total)) : T.live_watching
  })
  await api.on('content://done', async r => {
    state.reading = null; const s = await refreshSnapshot()
    if (r.read > 20) toast(T.content_done(fmt(s.status.content_files ?? r.with_text)))
    if (state.view === 'search' && state.query) runSearch(true)
  })
  await api.on('app://summon', () => { if (state.view !== 'search') go('search'); setTimeout(() => $('#q')?.select(), 30) })
  // "See all in Lumen" from the quick panel.
  await api.on('app://query', q => { state.query = q; state.kinds.clear(); state.view = 'search'; movePill(); render(); setTimeout(() => $('#q')?.focus(), 30) })
  await api.on('app://theme', applyTheme)
  await api.on('update-available', updateBanner)
  if (!isApp && location.hash.includes('update')) setTimeout(async () => updateBanner(await api.checkUpdate()), 600)
  const snap = await refreshSnapshot()
  applyTheme(!isApp && /light|dark/.test(location.hash) ? location.hash.match(/light|dark/)[0] : snap.config.theme)
  if (snap.firstRun) onboarding()
  else render()
  // demo hooks for previews: #scan, #done, #insights …
  const hash = location.hash.slice(1)
  if (!isApp && hash === 'scan') startFirstScan()
  if (!isApp && VIEWS[hash]) go(hash)
  if (!isApp && hash.startsWith('q=')) { state.query = decodeURIComponent(hash.slice(2)); render() }
}
boot()

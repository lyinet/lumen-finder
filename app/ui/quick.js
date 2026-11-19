// The quick panel: a Spotlight-style search box the shortcut pops up in the
// middle of the screen. Return opens, ⌘Return reveals, ⌘O / Tab shows every
// result in the main window, Esc (or clicking away) hides it.
import { api, isApp } from './bridge.js'
import { T } from './i18n.js'
import { ICON, esc, bytes, ago, highlight, kc, applyTheme } from './common.js'

if (!isApp) document.body.classList.add('browser')
const $ = s => document.querySelector(s)
const q = $('#qq'), list = $('#ql'), foot = $('#qfoot')
const ROWS = 8
let hits = [], total = 0, sel = 0, seq = 0, home = '', timer = 0

q.placeholder = T.quick_ph
const shortPath = p => !home || !p.startsWith(home) ? p
  : p.slice(home.length).startsWith('/Library/Mobile Documents/com~apple~CloudDocs') ? T.icloud_drive + p.slice(home.length + 45) : '~' + p.slice(home.length)

function row(f, terms, i) {
  const sub = f.snippet
    ? `<span class="qs"><b>${f.kind === 'image' || f.ocr ? T.in_image : T.in_text}</b>${highlight(f.snippet, terms)}</span>`
    : `<span>${esc(shortPath(f.parent))}</span>`
  return `<div class="qr ${i === sel ? 'on' : ''} ${f.offline ? 'off' : ''}" data-i="${i}">
    <div class="tile" style="--c:${kc(f.kind)}">${ICON.k[f.kind] || ICON.k.document}<em>${esc((f.ext || '').slice(0, 4))}</em></div>
    <div class="qt"><div class="qn">${highlight(f.name, terms)}</div><div class="qsub">${sub}</div></div>
    <div class="qm">${f.offline ? ICON.drive : f.cloud ? `${ICON.cloud}${ago(f.mtime)}` : `${bytes(f.size)} · ${ago(f.mtime)}`}</div></div>`
}

function draw() {
  const terms = q.value.trim().split(/\s+/).filter(Boolean)
  if (!hits.length) {
    list.innerHTML = q.value.trim() ? `<div class="qp-empty">${T.quick_none}</div>` : ''
  } else {
    list.innerHTML = (q.value.trim() ? '' : `<div class="qp-label">${T.quick_recent}</div>`) + hits.map((f, i) => row(f, terms, i)).join('')
  }
  foot.innerHTML = hits.length
    ? `<span><kbd>↵</kbd>${T.quick_hint_open}</span><span><kbd>⌘↵</kbd>${T.quick_hint_reveal}</span>` +
      (q.value.trim() ? `<button id="qall"><kbd>⌘O</kbd>${T.quick_all(total.toLocaleString())}</button>` : '')
    : ''
  $('#qall')?.addEventListener('click', showAll)
  foot.hidden = !hits.length
  fit()
}

/** Grow or shrink the native window to the content, so the glass never shows empty space. */
function fit() {
  const h = Math.ceil($('#qp').getBoundingClientRect().height)
  api.quickResize(h)
}

async function run() {
  const my = ++seq, query = q.value.trim()
  if (!query) {
    const r = await api.recent(6)
    if (my !== seq) return
    hits = r; total = r.length; $('#qtook').textContent = ''
  } else {
    const r = await api.search({ query, kinds: [], sort: 'relevance', limit: ROWS, offset: 0 })
    if (my !== seq) return
    hits = r.hits; total = r.total; $('#qtook').textContent = `${(r.took_us / 1000).toFixed(1)} ms`
  }
  sel = 0; draw()
}

const hide = () => api.quickHide()
async function openSel(reveal) {
  const f = hits[sel]; if (!f) return
  try { reveal ? await api.reveal(f.path) : await api.open(f.path); hide() } catch { run() }
}
function showAll() { api.showInMain(q.value.trim()); }

q.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(run, 40) })
addEventListener('keydown', e => {
  if (e.key === 'Escape') { e.preventDefault(); q.value ? (q.value = '', run()) : hide() }
  else if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) { e.preventDefault(); sel = Math.max(0, Math.min(hits.length - 1, sel + 1)); draw() }
  else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) { e.preventDefault(); sel = Math.max(0, sel - 1); draw() }
  else if (e.key === 'Enter') { e.preventDefault(); openSel(e.metaKey) }
  else if ((e.metaKey && e.key.toLowerCase() === 'o') || e.key === 'Tab') { e.preventDefault(); if (q.value.trim()) showAll() }
  else if (e.metaKey && e.key.toLowerCase() === 'y') { e.preventDefault(); hits[sel] && api.quickLook(hits[sel].path) }
  else if (e.metaKey && e.key.toLowerCase() === 'c' && !getSelection().toString()) { e.preventDefault(); hits[sel] && navigator.clipboard.writeText(hits[sel].path) }
})
list.addEventListener('mousemove', e => { const r = e.target.closest('.qr'); if (r && Number(r.dataset.i) !== sel) { sel = Number(r.dataset.i); list.querySelectorAll('.qr').forEach(x => x.classList.toggle('on', Number(x.dataset.i) === sel)) } })
list.addEventListener('click', e => { if (e.target.closest('.qr')) openSel(e.metaKey) })

function summon() {
  document.body.classList.remove('qp-in'); void document.body.offsetWidth; document.body.classList.add('qp-in')
  q.focus(); q.select(); run()
}

;(async () => {
  const snap = await api.snapshot(); home = snap.home; applyTheme(snap.config.theme)
  await api.on('quick://show', summon)
  // lumen://quick?q=…
  await api.on('app://query', v => { q.value = v; run() })
  await api.on('app://theme', applyTheme)
  const hash = decodeURIComponent(location.hash.slice(1))
  if (hash.startsWith('q=')) q.value = hash.slice(2)
  summon()
})()

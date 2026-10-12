// Helix Bridge UI, phase 4: read-only. No build step, no dependencies.
// Talks to the server's REST API (docs/api.md). Every device call runs one
// `tonepush` command on the server, about 1–3 s each, serialised.
'use strict';

const SLOTS = 9; // HX Effects block slots. Slot = chain position - 1 (position 1 is the input).
const RETRY_MS = 5000;
const GENERIC_NAME = 'New Preset';

// Block categories. The API does not send them yet, so the category is guessed
// from the model name. Unknown models get a neutral colour. Replace with HX
// Edit catalog data once the server exposes it.
const CATEGORIES = {
  dist: ['Distortion', 'DST'], dyn: ['Dynamics', 'DYN'], eq: ['EQ', 'EQ'],
  wah: ['Wah', 'WAH'], filter: ['Filter', 'FLT'], mod: ['Modulation', 'MOD'],
  delay: ['Delay', 'DLY'], reverb: ['Reverb', 'REV'], pitch: ['Pitch/Synth', 'PIT'],
  vol: ['Volume/Pan', 'VOL'], loop: ['Send/Return', 'LOOP'], looper: ['Looper', 'LPR'],
};
const CATEGORY_RULES = [
  [/looper/i, 'looper'],
  [/fx loop|send|return/i, 'loop'],
  [/wah|fassel|weeper|teardrop|conductor|throatbox/i, 'wah'],
  [/pitch|whammy|wham|octave|capo|detune|12 string|synth|osc|sustain|feedbacker/i, 'pitch'],
  [/gate|compress|comp\b|limiter|deluxe comp|autoswell/i, 'dyn'],
  [/verb|plate|hall|room|spring|chamber|shimmer|ganymede|searchlights|cave|glitz/i, 'reverb'],
  [/delay|echo|tape|transistor|bucket|elephant|cosmos|ducked/i, 'delay'],
  [/chorus|flang|phas|trem|vibe|rotary|leslie|ring mod|vibrato/i, 'mod'],
  [/eq\b|parametric|graphic|cali q|low.?cut|high.?cut/i, 'eq'],
  [/filter|mutant|autofilter|synth|seeker/i, 'filter'],
  [/volume|pan|gain block|stereo width/i, 'vol'],
  [/drive|dist|fuzz|boost|screamer|klon|minotaur|\brat\b|tycoctavia|kinky/i, 'dist'],
];

function category(model) {
  for (const [re, key] of CATEGORY_RULES) if (re.test(model)) return key;
  return null;
}

// ---------- State ----------

const state = {
  server: 'unknown', // unknown | ok | down
  device: 'unknown', // unknown | ok | missing | checking
  info: null, // /api/device
  presets: [], // /api/presets .presets
  chain: null, // /api/chain
  selectedSlot: null,
  loading: false,
  banner: null, // non-blocking error: { message, context }
  blocked: null, // blocking error: { icon, title, text, detail }
  browser: { open: false, filter: '', selected: null },
  connOpen: false,
};

// ---------- API ----------

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function api(path) {
  let res;
  try {
    res = await fetch(path, { cache: 'no-store' });
  } catch {
    throw new ApiError(0, 'unreachable', "Can't reach Helix Bridge server");
  }
  let body = null;
  try { body = await res.json(); } catch { /* not JSON */ }
  if (!res.ok) {
    throw new ApiError(res.status, (body && body.code) || 'http', (body && body.error) || `HTTP ${res.status}`);
  }
  return body;
}

// One refresh at a time: a second request while one is running reuses it.
let inflight = null;
let retryTimer = null;
let lastRefresh = 0;

// Each device call costs about 3–5 s on hardware (the server spaces USB
// sessions 3 s apart), so read the chain first: it names the loaded preset
// and fills most of the screen. A light refresh reads only the chain; preset
// names change only through writes, and the loaded one comes from the chain.
function refresh({ full = true } = {}) {
  if (!inflight) inflight = doRefresh(full || !state.presets.length).finally(() => { inflight = null; });
  return inflight;
}

/** Mark the preset the chain header names as loaded (labels are unique). */
function syncLoaded() {
  const label = state.chain?.label;
  if (!label || !state.presets.some((p) => p.label === label)) return;
  state.presets = state.presets.map((p) => (p.loaded === (p.label === label) ? p : { ...p, loaded: p.label === label }));
}

async function doRefresh(full) {
  clearTimeout(retryTimer);
  lastRefresh = Date.now();
  state.loading = true;
  // While a blocking screen is up, keep its red dots until a retry succeeds.
  if (!state.blocked && state.device !== 'ok') state.device = 'checking';
  render();
  try {
    await api('/api/health');
    state.server = 'ok';
    render();

    state.chain = await api('/api/chain');
    keepSelection();
    syncLoaded();
    state.device = 'ok';
    state.blocked = null;
    render();

    if (full) {
      state.presets = (await api('/api/presets')).presets;
      syncLoaded();
      if (state.browser.selected == null) state.browser.selected = loadedPreset()?.index ?? null;
      render();
    }
    if (!state.info) state.info = await api('/api/device');
    state.banner = null;
  } catch (e) {
    handleError(e);
  } finally {
    state.loading = false;
    render();
  }
}

function handleError(e) {
  const err = e instanceof ApiError ? e : new ApiError(0, 'client', String(e));
  if (err.code === 'unreachable') {
    state.server = 'down';
    state.device = 'unknown';
    state.blocked = {
      icon: 'LAN', title: "Can't reach Helix Bridge server",
      text: 'Check that the iPad is on the same Wi-Fi as the bridge and that the bridge is powered on.',
      detail: '',
    };
  } else if (err.code === 'no_device') {
    state.device = 'missing';
    state.blocked = {
      icon: 'USB', title: 'Device not connected',
      text: 'Check the USB cable between the HX Effects and the computer running Helix Bridge, and that the pedal is powered on.',
      detail: err.message,
    };
  } else if (state.chain || state.presets.length) {
    // Keep the last good state on screen; this is mid-song territory.
    const loaded = loadedPreset();
    state.banner = {
      message: err.message,
      context: loaded ? `Couldn't refresh. Still showing ${loaded.label}.` : "Couldn't refresh.",
    };
  } else {
    state.blocked = {
      icon: 'ERR', title: "Couldn't read the device",
      text: err.code === 'spawn'
        ? 'Helix Bridge could not start tonepush. Check the server configuration.'
        : 'The bridge answered, but reading from the pedal failed.',
      detail: err.message,
    };
    if (state.device === 'checking') state.device = 'unknown';
  }
  if (state.blocked) retryTimer = setTimeout(refresh, RETRY_MS);
}

// ---------- Derived data ----------

function loadedPreset() {
  return state.presets.find((p) => p.loaded) || null;
}

function bankOf(label) {
  return label ? label.slice(0, -1) : '';
}

/** Blocks keyed by slot (1..9). */
function slotMap() {
  const map = new Map();
  for (const b of state.chain?.blocks || []) map.set(b.position - 1, b);
  return map;
}

function keepSelection() {
  const map = slotMap();
  if (state.selectedSlot != null && map.has(state.selectedSlot)) return;
  const first = [...map.keys()].sort((a, b) => a - b)[0];
  state.selectedSlot = first ?? null;
}

/** Fill fraction for a display value, only where the scale is certain. */
function percentOf(value) {
  const m = /^(-?\d+(?:\.\d+)?)\s*%$/.exec(value.trim());
  if (!m) return null;
  return Math.max(0, Math.min(100, Number(m[1])));
}

// ---------- Rendering ----------

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function render() {
  const app = $('app');
  app.classList.toggle('is-loading', state.loading);
  app.classList.toggle('is-blocked', !!state.blocked);
  $('blocker').hidden = !state.blocked;
  renderConnection();
  renderPresetBar();
  renderBanner();
  renderChain();
  renderParams();
  renderBank();
  renderPresetList();
  renderStatusLine();
  renderBlocker();
  if (state.browser.open) renderBrowser();
}

function dotClass(kind) {
  if (kind === 'server') return { ok: 'ok', down: 'err', unknown: 'warn' }[state.server];
  if (state.server === 'down') return '';
  return { ok: 'ok', missing: 'err', checking: 'warn', unknown: '' }[state.device];
}

function deviceLabel() {
  if (state.device === 'checking') return 'Checking…';
  if (state.device === 'ok' && state.info) return `${state.info.name} ${state.info.firmware}`;
  return 'Device';
}

function renderConnection() {
  const pill = `<span class="conn-item"><span class="dot ${dotClass('server')}"></span>Server</span>` +
    `<span class="conn-item"><span class="dot ${dotClass('device')}"></span>${esc(deviceLabel())}</span>`;
  for (const el of document.querySelectorAll('.conn-pill')) el.innerHTML = pill;
  for (const el of document.querySelectorAll('.conn-dots')) {
    el.innerHTML = `<span class="dot ${dotClass('server')}"></span><span class="dot ${dotClass('device')}"></span>`;
  }

  const rows = [
    ['Server', { ok: 'reachable', down: 'unreachable', unknown: 'checking…' }[state.server]],
    ['Device', state.device === 'missing' ? 'not connected' : state.info?.name ?? '–'],
    ['Firmware', state.info?.firmware ?? '–'],
    ['Build', state.info?.build ?? '–'],
    ['Presets', state.info?.presets ?? '–'],
  ];
  $('conn-info').innerHTML = rows.map(([k, v]) => `<div><dt>${esc(k)}</dt><dd>${esc(v)}</dd></div>`).join('');
  $('conn-popover').hidden = !state.connOpen;
}

function renderPresetBar() {
  const loaded = loadedPreset();
  const label = state.chain?.label ?? loaded?.label ?? '–';
  const name = state.chain?.name ?? loaded?.name ?? '';
  $('nb-label').textContent = label;
  $('nb-name').textContent = name;
  $('nb-status').textContent = state.loading ? 'Loading from device…' : '';

  const names = state.chain?.snapshots?.length ? state.chain.snapshots : ['', '', '', ''];
  // The API does not say which snapshot is active, so none is highlighted.
  const snaps = names.map((n, i) =>
    `<div class="snap${state.chain ? '' : ' idle'}" title="${esc(n)}"><span>${i + 1}</span><span class="snap-name">${esc(n)}</span></div>`).join('');
  $('snaps-inline').innerHTML = snaps;
  $('snaps-row').innerHTML = snaps;

  const tempo = state.chain?.tempo;
  $('tempo').textContent = tempo == null ? '–' : (Number.isInteger(tempo) ? String(tempo) : tempo.toFixed(1));
}

function renderBanner() {
  const b = state.banner;
  $('banner').hidden = !b || !!state.blocked;
  if (!b) return;
  $('banner-msg').textContent = b.message;
  $('banner-msg').title = b.message;
  $('banner-ctx').textContent = b.context;
}

function renderChain() {
  const skeleton = !state.chain;
  const map = slotMap();
  let html = '<div class="io"><div class="io-bar"></div>In</div><div class="slots">';
  for (let n = 1; n <= SLOTS; n++) {
    if (skeleton) {
      html += '<div class="slot"><div class="slot-num"></div><div class="node-skel"></div><div class="slot-name"></div></div>';
      continue;
    }
    const b = map.get(n);
    if (!b) {
      html += `<div class="slot"><div class="slot-num">${n}</div><div class="node-empty"></div><div class="slot-name"></div></div>`;
      continue;
    }
    const cat = category(b.model);
    const code = cat ? CATEGORIES[cat][1] : 'FX';
    const cls = ['slot', b.enabled ? 'on' : '', n === state.selectedSlot ? 'selected' : ''].join(' ');
    html += `<button class="${cls}" type="button" data-slot="${n}" style="--c:var(--cat-${cat || 'unknown'})"` +
      ` aria-pressed="${n === state.selectedSlot}" aria-label="Slot ${n}: ${esc(b.model)}${b.enabled ? '' : ', bypassed'}">` +
      `<div class="slot-num">${n}</div><div class="node"><div class="node-code">${code}</div><div class="node-band"></div></div>` +
      `<div class="slot-name">${esc(b.model)}</div></button>`;
  }
  html += '</div><div class="io">Out</div>';
  $('chain').innerHTML = html;
}

function renderParams() {
  const el = $('params');
  if (!state.chain) {
    const cells = Array.from({ length: 6 }, () => '<div class="param"><div class="skel-line"></div><div class="track"></div></div>').join('');
    el.style.removeProperty('--c');
    el.innerHTML = `<div class="params-head"><div class="skel-title"></div></div><div class="params-grid">${cells}</div>`;
    return;
  }
  const b = slotMap().get(state.selectedSlot);
  if (!b) {
    el.innerHTML = '<div class="params-empty">No blocks in this preset.</div>';
    return;
  }
  const cat = category(b.model);
  el.style.setProperty('--c', `var(--cat-${cat || 'unknown'})`);
  const catName = cat ? CATEGORIES[cat][0] : 'Unknown category';
  const meta = `${catName} · slot ${state.selectedSlot} · ${b.enabled ? 'on' : 'bypassed'}`;
  const params = b.params.map((p) => {
    const pct = percentOf(p.value);
    const bar = pct == null ? '' :
      `<div class="track-fill" style="width:${pct}%"></div><div class="track-thumb" style="left:${pct}%"></div>`;
    return `<div class="param"><div class="param-top"><span class="param-name">${esc(p.name)}</span>` +
      `<span class="param-value">${esc(p.value)}</span></div><div class="track">${bar}</div></div>`;
  }).join('');
  el.innerHTML =
    `<div class="params-head"><div class="ph-code">${cat ? CATEGORIES[cat][1] : 'FX'}</div>` +
    `<div class="ph-model">${esc(b.model)}</div><div class="ph-meta">${esc(meta)}</div><div class="spacer"></div>` +
    `<button class="later ph-toggle" type="button" disabled title="Phase 6">${b.enabled ? 'Turn off' : 'Turn on'}</button></div>` +
    `<div class="params-grid">${params}</div>`;
}

function renderBank() {
  const loaded = loadedPreset();
  if (!loaded) {
    $('bank').innerHTML = '<div class="tile skel"></div>'.repeat(4);
    return;
  }
  const bank = bankOf(loaded.label);
  const tiles = state.presets.filter((p) => bankOf(p.label) === bank);
  $('bank').innerHTML = tiles.map((p) =>
    `<div class="tile${p.loaded ? ' loaded' : ''}"><div class="tile-top"><span class="tile-tag">${p.loaded ? 'LOADED' : ''}</span>` +
    `<span class="tile-label"><span>${esc(bank)}</span>${esc(p.label.slice(-1))}</span></div>` +
    `<div class="tile-name">${esc(p.name)}</div></div>`).join('');
}

let listKey = '';
function renderPresetList() {
  const key = state.presets.map((p) => `${p.label}${p.name}${p.loaded}`).join('|');
  if (key === listKey) return;
  listKey = key;
  const list = $('preset-list');
  list.innerHTML = state.presets.map((p) =>
    `<li class="pl-row${p.loaded ? ' loaded' : ''}${p.label.endsWith('A') && p.index > 0 ? ' bank-start' : ''}">` +
    `<span class="pl-label">${esc(p.label)}</span><span class="pl-name">${esc(p.name)}</span></li>`).join('');
  $('all-chip').textContent = state.presets.length ? `All ${state.presets.length}` : 'All';
  const row = list.querySelector('.loaded');
  if (row) row.scrollIntoView({ block: 'center' });
}

function renderStatusLine() {
  const i = state.info;
  $('statusline').innerHTML = i
    ? `<span>${esc(i.name)} · FW ${esc(i.firmware)} · BUILD ${esc(i.build)} · ${esc(i.presets)} PRESETS</span>`
    : '';
}

function renderBlocker() {
  const b = state.blocked;
  if (!b) return;
  $('blocker-icon').textContent = b.icon;
  $('blocker-title').textContent = b.title;
  $('blocker-text').textContent = b.text;
  $('blocker-detail').textContent = b.detail;
}

// ---------- Preset browser ----------

function matches(p, q) {
  if (!q) return true;
  return p.label.toLowerCase().includes(q) || p.name.toLowerCase().includes(q);
}

function renderBrowser() {
  const q = state.browser.filter.trim().toLowerCase();
  const banks = [];
  for (const p of state.presets) {
    const num = bankOf(p.label);
    if (!banks.length || banks[banks.length - 1].num !== num) banks.push({ num, presets: [] });
    banks[banks.length - 1].presets.push(p);
  }
  const rowHtml = (bk) => {
    const tiles = bk.presets.map((p) => {
      const cls = ['ptile',
        p.loaded ? 'loaded' : '',
        p.index === state.browser.selected ? 'selected' : '',
        p.name === GENERIC_NAME ? 'generic' : '',
        matches(p, q) ? '' : 'nomatch'].join(' ');
      return `<button class="${cls}" type="button" data-preset="${p.index}" aria-pressed="${p.index === state.browser.selected}">` +
        `<span class="ptile-letter">${esc(p.label.slice(-1))}</span><span class="ptile-name">${esc(p.name)}</span></button>`;
    }).join('');
    return `<div class="bank-row" id="bank-${esc(bk.num)}"><div class="bank-num">${esc(bk.num)}</div>${tiles}</div>`;
  };
  const visible = banks.filter((bk) => bk.presets.some((p) => matches(p, q)));
  let html;
  if (!state.presets.length) {
    html = '<div class="no-results">Presets not loaded yet.</div>';
  } else if (!visible.length) {
    html = `<div class="no-results">No preset matches “${esc(state.browser.filter)}”.</div>`;
  } else {
    // Three columns of banks (11/11/10 for all 32); portrait CSS flattens them into one list.
    const per = Math.ceil(visible.length / 3);
    html = [0, 1, 2].map((c) =>
      `<div class="bank-col">${visible.slice(c * per, (c + 1) * per).map(rowHtml).join('')}</div>`).join('');
  }
  $('banks').innerHTML = html;

  const jumpTargets = [];
  for (let n = 1; n <= banks.length; n += 4) jumpTargets.push(String(n).padStart(2, '0'));
  $('jumps').innerHTML = jumpTargets.map((n) =>
    `<button class="jump" type="button" data-jump="${n}">${n}</button>`).join('');
  updateActiveJump();

  const sel = state.presets.find((p) => p.index === state.browser.selected);
  const loaded = loadedPreset();
  $('sf-label').textContent = sel?.label ?? '';
  $('sf-name').textContent = sel?.name ?? '';
  $('sf-note').textContent = sel && loaded && sel.index !== loaded.index
    ? `selected · ${loaded.label} ${loaded.name} is loaded`
    : sel ? 'loaded' : '';
  $('load-btn').textContent = sel ? `Load ${sel.label}` : 'Load';
}

function updateActiveJump() {
  const banks = $('banks');
  const rows = [...banks.querySelectorAll('.bank-row')];
  const top = banks.getBoundingClientRect().top;
  const current = rows.find((r) => r.getBoundingClientRect().bottom > top + 4);
  const num = current ? Number(current.id.slice(5)) : 1;
  for (const j of document.querySelectorAll('.jump')) {
    const n = Number(j.dataset.jump);
    j.classList.toggle('active', num >= n && num < n + 4);
  }
}

function openBrowser() {
  state.browser.open = true;
  state.browser.selected = state.browser.selected ?? loadedPreset()?.index ?? null;
  state.connOpen = false;
  $('browser').hidden = false;
  renderConnection();
  renderBrowser();
  const sel = $('banks').querySelector('.selected') || $('banks').querySelector('.loaded');
  if (sel) sel.scrollIntoView({ block: 'center' });
  updateActiveJump();
}

function closeBrowser() {
  state.browser.open = false;
  $('browser').hidden = true;
}

// ---------- Events ----------

document.addEventListener('click', (e) => {
  const t = e.target.closest('button, [data-conn]');
  if (state.connOpen && !e.target.closest('#conn-popover, [data-conn]')) {
    state.connOpen = false;
    renderConnection();
  }
  if (!t) return;
  if (t.matches('[data-conn]')) {
    state.connOpen = !state.connOpen;
    renderConnection();
  } else if (t.matches('[data-retry]')) {
    state.connOpen = false;
    state.banner = null;
    refresh();
  } else if (t.id === 'banner-close') {
    state.banner = null;
    renderBanner();
  } else if (t.matches('[data-slot]')) {
    state.selectedSlot = Number(t.dataset.slot);
    renderChain();
    renderParams();
  } else if (t.matches('[data-open-browser]')) {
    openBrowser();
  } else if (t.matches('[data-close-browser]')) {
    closeBrowser();
  } else if (t.matches('[data-preset]')) {
    state.browser.selected = Number(t.dataset.preset);
    renderBrowser();
  } else if (t.matches('[data-jump]')) {
    const row = $(`bank-${t.dataset.jump}`);
    if (row) row.scrollIntoView({ block: 'start' });
    updateActiveJump();
  }
});

$('filter').addEventListener('input', (e) => {
  state.browser.filter = e.target.value;
  renderBrowser();
});

$('banks').addEventListener('scroll', updateActiveJump, { passive: true });

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (state.connOpen) { state.connOpen = false; renderConnection(); } else if (state.browser.open) closeBrowser();
});

// Re-read when the iPad comes back to the page (screen lock, app switch).
// No polling: each read is a USB session on the pedal.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && Date.now() - lastRefresh > RETRY_MS) refresh({ full: false });
});

render();
refresh();

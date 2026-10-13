// Helix Bridge UI. No build step, no dependencies.
// Talks to the server's REST API (docs/api.md). Every device call runs one
// `tonepush` command on the server, about 1–3 s each, serialised.
// Phase 6 writes: load preset, block on/off, parameter value, rename, save.
// They need the server's shared token (HELIX_TOKEN), asked for once and kept
// in this browser.
// Live updates: with the usb backend the server pushes the chain over a
// WebSocket (/api/live) whenever the pedal changes, footswitches included.
'use strict';

const SLOTS = 9; // HX Effects block slots. Slot = chain position - 1 (position 1 is the input).
const RETRY_MS = 5000;
const GENERIC_NAME = 'New Preset';
const MAX_NAME = 16; // HX preset name limit; the server checks it too.
const TOKEN_KEY = 'helix-bridge-token';

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
  writes: false, // server takes writes (/api/health .writes)
  live: false, // server pushes device changes (/api/health .live)
  unsaved: false, // edits made through the bridge since the last load or save
  busy: null, // write in progress: short label, e.g. 'Saving…'
};

// ---------- API ----------

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function api(path, { method = 'GET', body, token } = {}) {
  const headers = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (token) headers.Authorization = `Bearer ${token}`;
  let res;
  try {
    res = await fetch(path, {
      method, headers, cache: 'no-store',
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, 'unreachable', "Can't reach Helix Bridge server");
  }
  let data = null;
  try { data = await res.json(); } catch { /* not JSON */ }
  if (!res.ok) {
    throw new ApiError(res.status, (data && data.code) || 'http', (data && data.error) || `HTTP ${res.status}`);
  }
  return data;
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
    const health = await api('/api/health');
    state.writes = !!health.writes;
    state.live = !!health.live;
    connectLive();
    state.server = 'ok';
    render();

    state.chain = await api('/api/chain');
    state.unsaved = !!state.chain.unsaved;
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

// ---------- Live updates ----------

const LIVE_RETRY_MS = 3000;
let liveSocket = null;
let liveTimer = null;
let liveWasOpen = false;

function connectLive() {
  if (!state.live || liveSocket) return;
  clearTimeout(liveTimer);
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  let ws;
  try { ws = new WebSocket(`${scheme}://${location.host}/api/live`); } catch { return; }
  liveSocket = ws;
  ws.onopen = () => {
    // Changes made while the socket was down were missed: read once.
    if (liveWasOpen) refresh({ full: false });
    liveWasOpen = true;
  };
  ws.onmessage = (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    if (msg.type === 'chain') applyLiveChain(msg.chain);
    else if (msg.type === 'device') {
      if (msg.connected) refresh();
      else handleError(new ApiError(503, 'no_device', 'The pedal disconnected.'));
      render();
    }
  };
  ws.onclose = () => {
    if (liveSocket === ws) liveSocket = null;
    // Safari closes sockets when the iPad sleeps; visibilitychange reconnects.
    if (document.visibilityState === 'visible') liveTimer = setTimeout(connectLive, LIVE_RETRY_MS);
  };
}

function applyLiveChain(chain) {
  state.chain = chain;
  state.unsaved = !!chain.unsaved;
  state.device = 'ok';
  keepSelection();
  syncLoaded();
  render();
}

// ---------- Writes ----------

function storedToken() {
  try { return localStorage.getItem(TOKEN_KEY) || ''; } catch { return ''; }
}

function storeToken(token) {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token); else localStorage.removeItem(TOKEN_KEY);
  } catch { /* private mode: ask again next time */ }
}

let sessionToken = '';

function askToken(again) {
  const t = window.prompt(again
    ? 'That token was not accepted. Enter the Helix Bridge token (HELIX_TOKEN on the server):'
    : 'Enter the Helix Bridge token (HELIX_TOKEN on the server). It is kept in this browser.');
  return t ? t.trim() : '';
}

function canWrite() {
  return state.writes && !state.busy && !state.loading && state.device === 'ok' && !state.blocked;
}

/**
 * Send one write. Asks for the token when none is known or it is refused.
 * `then` picks what follows: 'chain' (re-read the chain), 'full' (also preset
 * names), or a function that applies the result locally with no device read.
 * A read costs 3–5 s on hardware, so cheap edits use the function.
 * Returns true on success.
 */
async function write(label, path, body, then = 'chain') {
  if (!canWrite()) return false;
  state.busy = label;
  render();
  let ok = false;
  let res = null;
  try {
    let token = sessionToken || storedToken();
    for (let attempt = 0; attempt < 2; attempt++) {
      if (!token) token = askToken(attempt > 0);
      if (!token) return false;
      try {
        res = await api(path, { method: 'POST', body, token });
        sessionToken = token;
        storeToken(token);
        state.unsaved = !!res.unsaved;
        ok = true;
        break;
      } catch (e) {
        if (!(e instanceof ApiError) || e.code !== 'unauthorized') throw e;
        sessionToken = '';
        storeToken('');
        token = '';
      }
    }
  } catch (e) {
    const err = e instanceof ApiError ? e : new ApiError(0, 'client', String(e));
    if (err.code === 'writes_disabled') state.writes = false;
    if (err.code === 'unreachable' || err.code === 'no_device') {
      handleError(err);
    } else {
      state.banner = { message: err.message, context: `${label.replace(/…$/, '')} failed.` };
    }
  } finally {
    state.busy = null;
    render();
  }
  if (ok && typeof then === 'function') {
    then(res);
    render();
  } else if (ok) {
    await refresh({ full: then === 'full' });
  }
  return ok;
}

function confirmDiscard() {
  return !state.unsaved || window.confirm('This preset has unsaved edits. Load another preset and lose them?');
}

async function loadPreset(index) {
  const p = state.presets.find((x) => x.index === index);
  if (!p || p.loaded || !confirmDiscard()) return false;
  return write(`Loading ${p.label}…`, '/api/select', { index });
}

function stepPreset(delta) {
  const loaded = loadedPreset();
  if (!loaded || !state.presets.length) return;
  const n = state.presets.length;
  loadPreset(state.presets[(loaded.index + delta + n) % n].index);
}

function toggleBlock() {
  const b = slotMap().get(state.selectedSlot);
  if (!b) return;
  const enabled = !b.enabled;
  write(enabled ? 'Turning on…' : 'Turning off…', `/api/blocks/${b.position}/enabled`, { enabled },
    () => { b.enabled = enabled; });
}

function editParam(index) {
  const b = slotMap().get(state.selectedSlot);
  const p = b?.params.find((x) => x.index === index);
  if (!p) return;
  const value = window.prompt(`${b.model} · ${p.name}\nNow ${p.value}. Type a value in the same units, e.g. ${p.value}:`, p.value);
  if (value == null || !value.trim() || value.trim() === p.value) return;
  write(`Setting ${p.name}…`, `/api/blocks/${b.position}/params/${p.index}`, { value: value.trim() },
    (res) => { p.value = res.value || value.trim(); });
}

function renamePreset() {
  const p = loadedPreset();
  if (!p) return;
  let name = window.prompt(`New name for ${p.label} (up to ${MAX_NAME} characters):`, p.name);
  if (name == null) return;
  name = name.trim();
  if (!name || name === p.name) return;
  if (name.length > MAX_NAME) {
    state.banner = { message: `Name is longer than ${MAX_NAME} characters.`, context: 'Not renamed.' };
    renderBanner();
    return;
  }
  write('Renaming…', `/api/presets/${p.index}/rename`, { name }, 'full');
}

function savePreset() {
  const p = loadedPreset();
  if (!p) return;
  if (!window.confirm(`Save the current edits over ${p.label} ${p.name}? This replaces the stored preset.`)) return;
  write('Saving…', '/api/save', {});
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
  renderControls();
  if (state.browser.open) renderBrowser();
}

const WRITES_OFF = 'Writes are off: set HELIX_TOKEN on the server';

/** Enable a static control when writes are possible; dashed and inert otherwise. */
function setControl(el, enabled, offTitle = WRITES_OFF) {
  el.disabled = !enabled;
  el.classList.toggle('later', !state.writes);
  el.title = state.writes ? '' : offTitle;
}

function renderControls() {
  const ready = canWrite() && !!loadedPreset();
  for (const el of document.querySelectorAll('[data-step]')) setControl(el, ready);
  setControl($('rename-btn'), ready);
  const save = $('save-btn');
  setControl(save, ready);
  save.classList.toggle('hot', state.writes && state.unsaved);
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
  $('nb-status').textContent = state.busy || (state.loading ? 'Loading from device…' : state.unsaved ? 'Unsaved' : '');
  $('namebar').classList.toggle('unsaved', state.unsaved);

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
  const live = canWrite();
  const params = b.params.map((p) => {
    const pct = percentOf(p.value);
    const bar = pct == null ? '' :
      `<div class="track-fill" style="width:${pct}%"></div><div class="track-thumb" style="left:${pct}%"></div>`;
    const inner = `<div class="param-top"><span class="param-name">${esc(p.name)}</span>` +
      `<span class="param-value">${esc(p.value)}</span></div><div class="track">${bar}</div>`;
    return state.writes
      ? `<button class="param editable" type="button" data-param="${p.index}"${live ? '' : ' disabled'} aria-label="${esc(p.name)}: ${esc(p.value)}. Tap to change">${inner}</button>`
      : `<div class="param">${inner}</div>`;
  }).join('');
  const toggle = state.writes
    ? `<button class="act ph-toggle" type="button" data-toggle${live ? '' : ' disabled'}>${b.enabled ? 'Turn off' : 'Turn on'}</button>`
    : `<button class="later ph-toggle" type="button" disabled title="${esc(WRITES_OFF)}">${b.enabled ? 'Turn off' : 'Turn on'}</button>`;
  el.innerHTML =
    `<div class="params-head"><div class="ph-code">${cat ? CATEGORIES[cat][1] : 'FX'}</div>` +
    `<div class="ph-model">${esc(b.model)}</div><div class="ph-meta">${esc(meta)}</div><div class="spacer"></div>` +
    `${toggle}</div>` +
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
  const live = canWrite();
  $('bank').innerHTML = tiles.map((p) => {
    const inner = `<div class="tile-top"><span class="tile-tag">${p.loaded ? 'LOADED' : ''}</span>` +
      `<span class="tile-label"><span>${esc(bank)}</span>${esc(p.label.slice(-1))}</span></div>` +
      `<div class="tile-name">${esc(p.name)}</div>`;
    return state.writes && !p.loaded
      ? `<button class="tile" type="button" data-load="${p.index}"${live ? '' : ' disabled'} aria-label="Load ${esc(p.label)} ${esc(p.name)}">${inner}</button>`
      : `<div class="tile${p.loaded ? ' loaded' : ''}">${inner}</div>`;
  }).join('');
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
  const load = $('load-btn');
  load.textContent = sel ? `Load ${sel.label}` : 'Load';
  setControl(load, canWrite() && !!sel && !sel.loaded);
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
  } else if (t.matches('[data-step]')) {
    stepPreset(Number(t.dataset.step));
  } else if (t.matches('[data-load]')) {
    loadPreset(Number(t.dataset.load));
  } else if (t.id === 'load-btn') {
    const index = state.browser.selected;
    closeBrowser();
    loadPreset(index);
  } else if (t.matches('[data-toggle]')) {
    toggleBlock();
  } else if (t.matches('[data-param]')) {
    editParam(Number(t.dataset.param));
  } else if (t.id === 'rename-btn') {
    renamePreset();
  } else if (t.id === 'save-btn') {
    savePreset();
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
  if (document.visibilityState !== 'visible') return;
  if (Date.now() - lastRefresh > RETRY_MS) refresh({ full: false });
  connectLive();
});

render();
refresh();

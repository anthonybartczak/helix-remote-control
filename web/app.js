// Helix Bridge UI. No build step, no dependencies.
// Talks to the server's REST API (docs/api.md). Every device call runs one
// `tonepush` command on the server, about 1–3 s each, serialised.
// Phase 6 writes: load preset, block on/off, parameter value, rename, save.
// They need the server's shared token (HELIX_TOKEN), asked for once and kept
// in this browser.
// Live updates: with the usb backend the server pushes the chain over a
// WebSocket (/api/live) whenever the pedal changes, footswitches included.
// Snapshots: tap one to switch (the active one is lit when the server reads
// the preset document: usb, fake). Tempo: TAP sets it from your taps, tapping
// the BPM types it; one write once the taps pause.
// Parameter controls: with HX Edit data on the server, continuous parameters
// are sliders, menus pickers and switches toggles (/api/models/{id}); writes
// go out by native value through LatestSender (latest.js), at most one in
// flight. Tapping a value still types it.
// Blocks: tap an empty slot to add an effect there (model picker from
// /api/catalog, HX Edit's categories and shelves); Remove takes the selected
// block out. Both change the edit buffer only, like any other edit.
// Footswitches: the block panel puts the selected block on FS1–FS6 or none
// (/api/blocks/{position}/footswitch); the add picker can do it in the same
// step. Only a block on a footswitch shows in the HX Effects' Stomp view.
'use strict';

const SLOTS = 9; // HX Effects block slots. Slot = chain position - 1 (position 1 is the input).
const RETRY_MS = 5000;
const GENERIC_NAME = 'New Preset';
const MAX_NAME = 16; // HX preset name limit; the server checks it too.
const TOKEN_KEY = 'helix-bridge-token';

// Block categories. With HX Edit's catalog the server sends each block's
// category, short label and colour (`category` in /api/chain), exactly as HX
// Edit draws them. Without it the category is guessed from the model name and
// drawn with the same names and palette (--cat-* in app.css); unknown models
// get a neutral colour.
const CATEGORIES = {
  dist: ['Distortion', 'Dist'], dyn: ['Dynamics', 'Dyn'], eq: ['EQ', 'EQ'],
  wah: ['Wah', 'Wah'], filter: ['Filter', 'Filt'], mod: ['Modulation', 'Mod'],
  delay: ['Delay', 'Delay'], reverb: ['Reverb', 'Verb'], pitch: ['Pitch/Synth', 'Pitch'],
  vol: ['Volume/Pan', 'Vol'], loop: ['Send/Return', 'Snd/Rtn'], looper: ['Looper', 'Loop'],
};
const CATEGORY_RULES = [
  [/looper/i, 'looper'],
  [/fx loop|send|return/i, 'loop'],
  [/wah|fassel|weeper|teardrop|conductor|throatbox/i, 'wah'],
  [/pitch|whammy|wham|octave|capo|detune|12 string|synth|osc|sustain|feedbacker/i, 'pitch'],
  [/gate|compress|comp\b|limiter|deluxe comp|autoswell|squeeze/i, 'dyn'],
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

/** A block's category as { name, short, colour, subcategory }: the server's, else a guess. */
function blockCategory(b) {
  const c = b.category;
  if (c && /^#[0-9a-f]{6}$/i.test(c.colour)) {
    return { name: c.name, short: c.short, colour: c.colour, subcategory: c.subcategory ?? null };
  }
  const key = category(b.model);
  if (!key) return { name: 'Unknown category', short: 'FX', colour: 'var(--cat-unknown)', subcategory: null };
  return { name: CATEGORIES[key][0], short: CATEGORIES[key][1], colour: `var(--cat-${key})`, subcategory: null };
}

/** "Line 6 HX Effects": every device TonePush supports is a Line 6 HX. */
function deviceName(info) {
  return info?.name ? `Line 6 ${info.name}` : 'Line 6 HX';
}

// ---------- State ----------

const state = {
  server: 'unknown', // unknown | ok | down
  device: 'unknown', // unknown | ok | missing | checking
  info: null, // /api/device
  presets: [], // /api/presets .presets
  chain: null, // /api/chain
  selectedSlot: null, // 1..SLOTS; may be an empty slot
  selectionLabel: null, // preset label the selection belongs to
  catalog: null, // /api/catalog .categories | CATALOG_PENDING | { error }
  // selected: model id; shelf: All | Mono | Stereo | Legacy, kept in this browser
  picker: { open: false, slot: null, category: 0, filter: '', selected: null, shelf: 'All', fs: 'none' },
  loading: false,
  banner: null, // non-blocking error: { message, context }
  blocked: null, // blocking error: { icon, title, text, detail }
  browser: { open: false, filter: '', selected: null },
  connOpen: false,
  writes: false, // server takes writes (/api/health .writes)
  live: false, // server pushes device changes (/api/health .live)
  unsaved: false, // edits made through the bridge since the last load or save
  busy: null, // write in progress: short label, e.g. 'Saving…'
  models: new Map(), // model_id -> /api/models/{id} | MODEL_PENDING | null (no data: type values)
  drag: null, // slider under a finger: { key, el, pointer, x0, i0, i, started, ctx }
  tempoPending: null, // BPM tapped or typed, not yet confirmed by the server
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
    loadModels();
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
  loadModels();
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
async function write(label, path, body, then = 'chain', method = 'POST') {
  if (!canWrite()) return false;
  state.busy = label;
  render();
  let ok = false;
  let res = null;
  let stale = false;
  try {
    let token = sessionToken || storedToken();
    for (let attempt = 0; attempt < 2; attempt++) {
      if (!token) token = askToken(attempt > 0);
      if (!token) return false;
      try {
        res = await api(path, { method, body, token });
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
      // The chain changed under us (409): show what is there now.
      if (err.code === 'stale') stale = true;
    }
  } finally {
    state.busy = null;
    render();
  }
  if (ok && typeof then === 'function') {
    then(res);
    render();
  } else if (ok || stale) {
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
  pins.delete(paramKey(b, p));
  const value = window.prompt(`${b.model} · ${p.name}\nNow ${p.value}. Type a value in the same units, e.g. ${p.value}:`, p.value);
  if (value == null || !value.trim() || value.trim() === p.value) return;
  write(`Setting ${p.name}…`, `/api/blocks/${b.position}/params/${p.index}`, { value: value.trim() },
    (res) => { p.value = res.value || value.trim(); });
}

// ---------- Adding and removing blocks ----------

const CATALOG_PENDING = 'pending';
// The pedal puts a block added on its front panel on a free footswitch; a
// block added over USB (op40) gets none, and the HX Effects home
// screen draws only footswitch blocks. Verified 2026-10-15 on the HX Effects.
const NO_FOOTSWITCH = 'New blocks get no footswitch: on the pedal they show in Signal Flow (MENU → Signal Flow).';
const PICK_FOOTSWITCH = 'Pick a footswitch to see it in the pedal’s Stomp view too.';
// What the add picker does about a footswitch: 'none', 'free' (the first
// switch carrying nothing, as the front panel does) or a number. Kept in this
// browser.
const ADD_FS_KEY = 'helix-bridge-add-footswitch';

/** Fetch the model list once (catalog data only, no device call); again after a failure. */
function loadCatalog() {
  if (state.catalog === CATALOG_PENDING || Array.isArray(state.catalog)) return;
  state.catalog = CATALOG_PENDING;
  api('/api/catalog').then(
    (c) => { state.catalog = c.categories; },
    (e) => {
      state.catalog = {
        error: e.code === 'no_catalog'
          ? 'Adding blocks needs HX Edit data on the server (/vendor, usb backend).'
          : e.message,
      };
    },
  ).finally(() => { if (state.picker.open) renderPicker(); });
}

const SHELF_ALL = 'All';
const SHELF_KEY = 'helix-bridge-shelf';

function storedShelf() {
  try { return localStorage.getItem(SHELF_KEY) || SHELF_ALL; } catch { return SHELF_ALL; }
}

function storeShelf(shelf) {
  try { localStorage.setItem(SHELF_KEY, shelf); } catch { /* private mode: forget it */ }
}

/**
 * A category's models in sections: by effect type when the server sends
 * types (Boost, Overdrive, Fuzz…; ours, not Line 6's), else by shelf.
 * Returns [name, models] pairs, empty sections left out.
 */
function sections(c, list) {
  const keys = c.types?.length ? c.types : [...new Set(list.map((m) => m.shelf ?? ''))];
  const keyOf = c.types?.length ? (m) => m.type : (m) => m.shelf ?? '';
  return keys.map((k) => [k, list.filter((m) => keyOf(m) === k)]).filter(([, ms]) => ms.length);
}

/** Same-named models side by side (Mono, then Stereo), else catalog order. */
function byName(list) {
  const first = new Map();
  for (const m of list) if (!first.has(m.name)) first.set(m.name, first.size);
  return [...list].sort((a, b) => first.get(a.name) - first.get(b.name));
}

/** Every model in the list, with its category and shelf. */
function catalogModels() {
  if (!Array.isArray(state.catalog)) return [];
  return state.catalog.flatMap((c, ci) => c.shelves.flatMap((s) =>
    s.models.map((m) => ({ ...m, category: c, ci, shelf: s.name ?? null }))));
}

function selectSlot(n) {
  state.selectedSlot = n;
  renderChain();
  renderParams();
  if (!slotMap().has(n) && canWrite()) openPicker(n);
}

function openPicker(slot) {
  state.picker = { ...state.picker, open: true, slot, filter: '', selected: null, shelf: storedShelf(), fs: storedAddFs() };
  state.connOpen = false;
  $('model-filter').value = '';
  $('picker').hidden = false;
  loadCatalog();
  renderConnection();
  renderPicker();
  $('models').scrollTop = 0;
  // Portrait shows categories as a strip: bring the last one used into view.
  document.querySelector('.cat.active')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

function closePicker() {
  state.picker.open = false;
  $('picker').hidden = true;
}

async function addBlock() {
  const { slot, selected } = state.picker;
  const m = catalogModels().find((x) => x.id === selected);
  if (slot == null || !m || slotMap().has(slot) || !canWrite()) return;
  const fs = addFootswitch();
  closePicker();
  state.selectedSlot = slot;
  const added = await write(`Adding ${m.name}…`, `/api/blocks/${slot + 1}`, { model: m.id });
  if (added && fs) {
    await write(`Putting ${m.name} on FS${fs}…`, `/api/blocks/${slot + 1}/footswitch`, { switch: fs, model: m.id });
  }
}

// ---------- Footswitches ----------

/** The chain's footswitches, or null when the backend does not read them. */
function footswitches() {
  return state.chain?.footswitches ?? null;
}

/** Model names of the blocks on switch `s`, leaving out `position`. */
function carriedBy(s, position) {
  const blocks = state.chain?.blocks || [];
  return s.blocks.filter((p) => p !== position)
    .map((p) => blocks.find((b) => b.position === p)?.model ?? `slot ${p - 1}`);
}

function storedAddFs() {
  try { return localStorage.getItem(ADD_FS_KEY) || 'none'; } catch { return 'none'; }
}

function storeAddFs(value) {
  try { localStorage.setItem(ADD_FS_KEY, value); } catch { /* private mode: forget it */ }
}

/** The switch the add picker's choice means now, or null for none. */
function addFootswitch() {
  const switches = footswitches();
  const choice = state.picker.fs;
  if (!switches || choice === 'none') return null;
  if (choice === 'free') return switches.find((s) => !s.blocks.length)?.switch ?? null;
  const n = Number(choice);
  return switches.some((s) => s.switch === n) ? n : null;
}

function setFootswitch(sw) {
  const b = slotMap().get(state.selectedSlot);
  if (!b || (b.footswitch ?? null) === sw) return;
  const body = { switch: sw };
  // The model we show: if the pedal now has another one there, the server refuses (409).
  if (b.model_id != null) body.model = b.model_id;
  write(sw ? `Putting ${b.model} on FS${sw}…` : `Taking ${b.model} off FS${b.footswitch}…`,
    `/api/blocks/${b.position}/footswitch`, body);
}

/** The add picker's footswitch menu; hidden when the backend has no footswitches. */
function renderAddFootswitch() {
  const wrap = $('pf-fs-wrap');
  const switches = footswitches();
  wrap.hidden = !switches || !state.writes;
  if (wrap.hidden) return;
  const free = switches.find((s) => !s.blocks.length);
  const opt = (value, label) => `<option value="${value}"${String(state.picker.fs) === String(value) ? ' selected' : ''}>${esc(label)}</option>`;
  $('pf-fs').innerHTML = [
    opt('none', 'No footswitch'),
    opt('free', free ? `First free (FS${free.switch})` : 'First free (none free)'),
    ...switches.map((s) => {
      const others = carriedBy(s, null);
      return opt(s.switch, others.length ? `FS${s.switch} · ${others.join(', ')}` : `FS${s.switch}`);
    }),
  ].join('');
}

/** The block panel's footswitch row: None, FS1…FS6, and what it means. */
function footswitchRow(b, live) {
  const switches = footswitches();
  if (!switches) return '';
  const on = b.footswitch ?? null;
  const off = state.writes && live ? '' : ' disabled';
  const btn = (n, label, title) => `<button class="seg-btn fs-btn${on === n ? ' active' : ''}" type="button" data-fs="${n ?? 0}"` +
    ` aria-pressed="${on === n}" title="${esc(title)}"${off}>${label}</button>`;
  const buttons = [btn(null, 'None', 'No footswitch')].concat(switches.map((s) => {
    const others = carriedBy(s, b.position);
    const name = s.label ? `FS${s.switch} (${s.label})` : `FS${s.switch}`;
    const title = others.length ? `${name}, also switches ${others.join(', ')}` : name;
    return btn(s.switch, `FS${s.switch}${others.length ? '<span class="fs-taken" aria-hidden="true"></span>' : ''}`, title);
  })).join('');
  let note;
  if (on == null) {
    note = 'No footswitch: on the pedal it shows in Signal Flow only.';
  } else {
    const others = carriedBy(switches.find((s) => s.switch === on) ?? { blocks: [] }, b.position);
    note = `Shows in Stomp view on FS${on}.${others.length ? ` FS${on} also switches ${others.join(', ')}.` : ''}`;
  }
  return `<div class="fs-row"><span class="fs-label">Footswitch</span>` +
    `<div class="seg fs-seg" role="group" aria-label="Footswitch">${buttons}</div>` +
    `<span class="fs-note">${esc(note)}</span></div>`;
}

function removeBlock() {
  const slot = state.selectedSlot;
  const b = slotMap().get(slot);
  if (!b) return;
  if (!window.confirm(`Remove ${b.model} from slot ${slot}?\nThe preset stays unsaved until you save it.`)) return;
  // The model we show: if the pedal now has another one there, the server refuses (409).
  const seen = b.model_id != null ? `?model=${b.model_id}` : '';
  write(`Removing ${b.model}…`, `/api/blocks/${b.position}${seen}`, undefined, 'chain', 'DELETE');
}

// ---------- Parameter controls ----------

const MODEL_PENDING = 'pending';
const SLIDE_GAP_MS = 100; // least time between two slider writes (latest.js)
const PIN_HOLD_MS = 1500; // after the last write, ignore older echoes this long
const DRAG_START_PX = 6; // horizontal travel before a touch counts as a drag

/** Fetch metadata for models in the chain not seen yet. Catalog only, no USB. */
function loadModels() {
  for (const b of state.chain?.blocks || []) {
    const id = b.model_id;
    if (id == null || state.models.has(id)) continue;
    state.models.set(id, MODEL_PENDING);
    api(`/api/models/${id}`).then((m) => {
      state.models.set(id, m);
      renderParams();
    }, (e) => {
      // No data for it: type values. Network trouble: try again next chain.
      if (e instanceof ApiError && e.code === 'no_catalog') state.models.set(id, null);
      else state.models.delete(id);
    });
  }
}

const paramKey = (b, p) => `${b.position}/${p.index}`;

/** The control for a parameter, or null to keep tap-to-type only. */
function controlOf(b, p) {
  const m = state.models.get(b.model_id);
  if (!m || m === MODEL_PENDING || typeof p.native !== 'number') return null;
  const c = m.params.find((x) => x.index === p.index);
  if (!c || c.kind === 'text' || c.labels.length !== c.steps + 1) return null;
  // A menu value without a name (left off the list) is typed, not shown as its neighbour.
  if (c.kind === 'menu' && (p.native < c.min - 0.5 || p.native > c.max + 0.5)) return null;
  return c;
}

/** Native value at grid position i; the server has the same formula (controls.rs). */
function nativeAt(c, i) {
  if (i >= c.steps) return c.max;
  const t = i / c.steps;
  return c.scale === 'log' ? c.min * Math.pow(c.max / c.min, t) : c.min + t * (c.max - c.min);
}

/** Nearest grid position to a native value. */
function indexOf(c, native) {
  let t = c.scale === 'log'
    ? Math.log(native / c.min) / Math.log(c.max / c.min)
    : (native - c.min) / (c.max - c.min);
  if (!Number.isFinite(t)) t = 0;
  return Math.max(0, Math.min(c.steps, Math.round(t * c.steps)));
}

// What this client last sent per parameter. While writes are in flight, and
// briefly after, chain updates that disagree are older echoes: the control
// keeps the pinned value instead of jumping under the finger.
const pins = new Map(); // key -> { model, native, label, until }

function shownValue(b, p) {
  const key = paramKey(b, p);
  const pin = pins.get(key);
  if (pin && pin.model === b.model_id) {
    const active = sender.busy(key) || state.drag?.key === key;
    const agrees = typeof p.native === 'number' &&
      Math.abs(p.native - pin.native) <= 1e-4 * Math.max(1, Math.abs(pin.native));
    if ((active || Date.now() < pin.until) && !agrees) return { native: pin.native, label: pin.label };
    if (!active) pins.delete(key);
  } else if (pin) {
    pins.delete(key);
  }
  return { native: p.native, label: p.value };
}

function findParam(position, index) {
  const b = state.chain?.blocks.find((x) => x.position === position);
  return b ? [b, b.params.find((x) => x.index === index)] : [null, null];
}

const sender = new LatestSender(
  (key, v) => api(`/api/blocks/${v.position}/params/${v.index}`, {
    method: 'POST', body: { native: v.native, model: v.model }, token: sessionToken,
  }),
  {
    gap: SLIDE_GAP_MS,
    onResult(key, v, res) {
      storeToken(sessionToken);
      state.unsaved = !!res.unsaved;
      const [b, p] = findParam(v.position, v.index);
      if (p && b.model_id === v.model) {
        p.native = v.native;
        if (res.value) p.value = res.value;
      }
      renderPresetBar();
      renderControls();
    },
    onError(key, v, e) {
      sender.cancel(key);
      pins.delete(key);
      controlError(e, 'Value not set.');
    },
    onIdle(key) {
      const pin = pins.get(key);
      if (pin) pin.until = Date.now() + PIN_HOLD_MS;
      if (!state.drag) renderParams();
      setTimeout(() => { if (!state.drag) renderParams(); }, PIN_HOLD_MS + 50);
    },
  },
);

/** Show a failed control write (slider, tempo) without blocking the screen. */
function controlError(e, context) {
  const err = e instanceof ApiError ? e : new ApiError(0, 'client', String(e));
  if (err.code === 'unauthorized') {
    sessionToken = '';
    storeToken('');
    state.banner = { message: 'Token not accepted.', context: 'Touch the control again to enter it.' };
  } else if (err.code === 'unreachable' || err.code === 'no_device') {
    handleError(err);
  } else {
    if (err.code === 'writes_disabled') state.writes = false;
    state.banner = { message: err.message, context };
    if (err.code === 'stale') refresh({ full: false });
  }
  if (!state.drag) render(); else renderBanner();
}

/** The token for a control write, asking once if none is known. */
function controlToken() {
  if (!sessionToken) sessionToken = storedToken() || askToken(false);
  return sessionToken;
}

/** Context of a control element: block, parameter, metadata. */
function controlContext(el) {
  const b = slotMap().get(state.selectedSlot);
  const p = b?.params.find((x) => x.index === Number(el.dataset.ctl));
  const c = p && controlOf(b, p);
  return c ? { b, p, c, key: paramKey(b, p) } : null;
}

/** Queue a write of grid position i (last value wins, see latest.js). */
function sendIndex(ctx, i) {
  const { b, p, c, key } = ctx;
  const native = nativeAt(c, i);
  pins.set(key, { model: b.model_id, native, label: c.labels[i], until: Infinity });
  sender.push(key, { position: b.position, index: p.index, native, model: b.model_id });
}

/** Move a slider's drawing to grid position i without re-rendering. */
function paintSlider(el, c, i) {
  el.style.setProperty('--pos', `${(i / c.steps) * 100}%`);
  el.setAttribute('aria-valuenow', String(i));
  el.setAttribute('aria-valuetext', c.labels[i]);
  const value = el.closest('.param')?.querySelector('.param-value');
  if (value) value.textContent = c.labels[i];
}

function controlReady() {
  return canWrite() && !!controlToken();
}

/** Like controlReady, but a drag never starts behind a token prompt. */
function dragReady() {
  if (!canWrite()) return false;
  if (sessionToken || (sessionToken = storedToken())) return true;
  controlToken(); // asks; the next touch drags
  return false;
}

function sliderDown(e) {
  const el = e.target.closest('[data-slider]');
  if (!el || state.drag || el.getAttribute('aria-disabled') === 'true') return;
  const ctx = controlContext(el);
  if (!ctx || !dragReady()) return;
  const i = Number(el.getAttribute('aria-valuenow'));
  state.drag = { key: ctx.key, el, pointer: e.pointerId, x0: e.clientX, i0: i, i, started: false, ctx };
  try { el.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ }
}

function sliderMove(e) {
  const d = state.drag;
  if (!d || e.pointerId !== d.pointer) return;
  const dx = e.clientX - d.x0;
  if (!d.started) {
    if (Math.abs(dx) < DRAG_START_PX) return;
    d.started = true;
    d.el.classList.add('dragging');
  }
  const width = d.el.clientWidth || 1;
  const i = Math.max(0, Math.min(d.ctx.c.steps, Math.round(d.i0 + (dx / width) * d.ctx.c.steps)));
  if (i === d.i) return;
  d.i = i;
  paintSlider(d.el, d.ctx.c, i);
  sendIndex(d.ctx, i);
}

function sliderUp(e) {
  const d = state.drag;
  if (!d || e.pointerId !== d.pointer) return;
  state.drag = null;
  d.el.classList.remove('dragging');
  try { d.el.releasePointerCapture(d.pointer); } catch { /* released already */ }
  render(); // what arrived during the drag
}

function sliderKey(e) {
  const el = e.target.closest('[data-slider]');
  const step = { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1 }[e.key];
  if (!el || !step || el.getAttribute('aria-disabled') === 'true') return;
  e.preventDefault();
  const ctx = controlContext(el);
  if (!ctx || !controlReady()) return;
  const i = Math.max(0, Math.min(ctx.c.steps, Number(el.getAttribute('aria-valuenow')) + step));
  paintSlider(el, ctx.c, i);
  sendIndex(ctx, i);
}

function menuChange(e) {
  const el = e.target.closest('[data-menu]');
  if (!el) return;
  const ctx = controlContext(el);
  if (!ctx || !controlReady()) { renderParams(); return; }
  sendIndex(ctx, Number(el.value));
}

function switchToggle(el) {
  const ctx = controlContext(el);
  if (!ctx || !controlReady()) return;
  const on = el.getAttribute('aria-pressed') === 'true';
  const i = on ? 0 : 1;
  el.setAttribute('aria-pressed', String(!on));
  el.querySelector('.switch-label').textContent = ctx.c.labels[i];
  sendIndex(ctx, i);
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

// ---------- Snapshots and tempo ----------

const TAP_RESET_MS = 2000; // a longer gap starts a new series of taps (as TonePush)
const TAP_KEEP = 5; // the last five taps count
const TAP_SEND_MS = 1500; // send the tapped tempo once the taps pause this long
const taps = [];
let tapTimer = null;

function selectSnapshot(index) {
  if (!state.chain || state.chain.snapshot === index) return;
  // The server pushes the new chain (blocks on/off, values) over /api/live;
  // without live updates, read it.
  write(`Snapshot ${index + 1}…`, '/api/snapshot', { index },
    state.live ? () => { state.chain.snapshot = index; } : 'chain');
}

/** BPM from the taps so far, once there are two; null before. */
function tappedBpm(now) {
  if (taps.length && now - taps[taps.length - 1] > TAP_RESET_MS) taps.length = 0;
  taps.push(now);
  if (taps.length > TAP_KEEP) taps.shift();
  if (taps.length < 2) return null;
  const bpm = (60000 * (taps.length - 1)) / (now - taps[0]);
  return Math.round(Math.min(240, Math.max(40, bpm)) * 10) / 10;
}

function tap() {
  if (!state.chain || !controlReady()) return;
  const bpm = tappedBpm(performance.now());
  const btn = $('tap-btn');
  btn.classList.remove('flash');
  void btn.offsetWidth; // restart the flash animation
  btn.classList.add('flash');
  if (bpm == null) return;
  state.tempoPending = bpm;
  renderPresetBar();
  clearTimeout(tapTimer);
  // One write per tap series: the tempo rewrites the whole preset document.
  tapTimer = setTimeout(() => setTempo(bpm), TAP_SEND_MS);
}

function typeTempo() {
  if (!state.chain || !controlReady()) return;
  const now = state.chain.tempo;
  const text = window.prompt(`Tempo in BPM, 40 to 240. Now ${now ?? '–'}:`, now ?? '');
  if (text == null || !text.trim()) return;
  const bpm = Number(text.trim().replace(',', '.'));
  if (!Number.isFinite(bpm) || bpm < 40 || bpm > 240) {
    state.banner = { message: `${text.trim()} is not a tempo between 40 and 240 BPM.`, context: 'Tempo not set.' };
    renderBanner();
    return;
  }
  setTempo(Math.round(bpm * 10) / 10);
}

const tempoSender = new LatestSender(
  (key, bpm) => api('/api/tempo', { method: 'POST', body: { bpm }, token: sessionToken }),
  {
    gap: SLIDE_GAP_MS,
    onResult(key, bpm, res) {
      storeToken(sessionToken);
      state.unsaved = !!res.unsaved;
      if (state.chain) state.chain.tempo = bpm;
      if (state.tempoPending === bpm) state.tempoPending = null;
      renderPresetBar();
      renderControls();
    },
    onError(key, bpm, e) {
      tempoSender.cancel(key);
      state.tempoPending = null;
      controlError(e, 'Tempo not set.');
    },
  },
);

function setTempo(bpm) {
  state.tempoPending = bpm;
  renderPresetBar();
  tempoSender.push('tempo', bpm);
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

/**
 * Keep the selected slot while the same preset stays loaded, even once it is
 * empty (a block just removed: offer to add another). Another preset starts
 * on its first block, or on slot 1 if it has none.
 */
function keepSelection() {
  const map = slotMap();
  const label = state.chain?.label ?? null;
  const samePreset = label === state.selectionLabel;
  state.selectionLabel = label;
  const s = state.selectedSlot;
  if (s != null && (map.has(s) || (samePreset && s >= 1 && s <= SLOTS))) return;
  const first = [...map.keys()].sort((a, b) => a - b)[0];
  state.selectedSlot = first ?? (state.chain ? 1 : null);
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
  if (state.picker.open) renderPicker();
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
  const tempoReady = canWrite() && !!state.chain;
  setControl($('tap-btn'), tempoReady);
  $('tempo-btn').disabled = !tempoReady;
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
  if (state.device === 'ok' && state.info) return `${deviceName(state.info)} ${state.info.firmware}`;
  return 'Device';
}

function renderConnection() {
  const pill = `<span class="conn-item"><span class="dot ${dotClass('server')}"></span>Server</span>` +
    `<span class="conn-item"><span class="dot ${dotClass('device')}"></span>${esc(deviceLabel())}</span>`;
  for (const el of document.querySelectorAll('.conn-pill')) el.innerHTML = pill;
  const name = state.info ? deviceName(state.info) : '';
  for (const el of document.querySelectorAll('.conn-dots')) {
    el.innerHTML = `<span class="dot ${dotClass('server')}"></span><span class="dot ${dotClass('device')}"></span>` +
      (name ? `<span class="conn-name">${esc(name)}</span>` : '');
  }
  $('device-name').textContent = state.info ? `${name} · FW ${state.info.firmware}` : '';

  const rows = [
    ['Server', { ok: 'reachable', down: 'unreachable', unknown: 'checking…' }[state.server]],
    ['Device', state.device === 'missing' ? 'not connected' : state.info ? deviceName(state.info) : '–'],
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
  // Lit only when the server knows (usb, fake); `cli` cannot tell.
  const active = state.chain?.snapshot;
  const live = canWrite();
  const snaps = names.map((n, i) => {
    const cls = ['snap', state.chain ? '' : 'idle', i === active ? 'active' : ''].join(' ');
    const inner = `<span>${i + 1}</span><span class="snap-name">${esc(n)}</span>`;
    return state.writes && state.chain
      ? `<button class="${cls}" type="button" data-snapshot="${i}" aria-pressed="${i === active}" aria-label="Snapshot ${i + 1}: ${esc(n)}" title="${esc(n)}"${live ? '' : ' disabled'}>${inner}</button>`
      : `<div class="${cls}" title="${esc(n)}">${inner}</div>`;
  }).join('');
  $('snaps-inline').innerHTML = snaps;
  $('snaps-row').innerHTML = snaps;

  const tempo = state.tempoPending ?? state.chain?.tempo;
  $('tempo').textContent = tempo == null ? '–' : (Number.isInteger(tempo) ? String(tempo) : tempo.toFixed(1));
  $('tempo-btn').classList.toggle('pending', state.tempoPending != null);
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
      const sel = n === state.selectedSlot;
      html += `<button class="slot empty${sel ? ' selected' : ''}" type="button" data-slot="${n}" aria-pressed="${sel}"` +
        ` aria-label="Slot ${n}: empty${state.writes ? '. Tap to add a block' : ''}">` +
        `<div class="slot-num">${n}</div><div class="node-empty"></div><div class="slot-name"></div></button>`;
      continue;
    }
    const cat = blockCategory(b);
    const cls = ['slot', b.enabled ? 'on' : '', n === state.selectedSlot ? 'selected' : ''].join(' ');
    const fs = b.footswitch ? ` · <span class="slot-fs">FS${b.footswitch}</span>` : '';
    html += `<button class="${cls}" type="button" data-slot="${n}" style="--c:${cat.colour}"` +
      ` aria-pressed="${n === state.selectedSlot}" aria-label="Slot ${n}: ${esc(b.model)}, ${esc(cat.name)}${b.based_on ? `, based on ${esc(b.based_on)}` : ''}${b.enabled ? '' : ', bypassed'}${b.footswitch ? `, on footswitch ${b.footswitch}` : ''}"${b.based_on ? ` title="${esc(b.model)}: ${esc(b.based_on)}"` : ''}>` +
      `<div class="slot-num">${n}${fs}</div><div class="node"><div class="node-code">${esc(cat.short)}</div><div class="node-band"></div></div>` +
      `<div class="slot-name">${esc(b.model)}</div></button>`;
  }
  html += '</div><div class="io">Out</div>';
  $('chain').innerHTML = html;
}

function renderParams() {
  if (state.drag) return; // never rebuild a slider under the finger; sliderUp renders
  const el = $('params');
  if (!state.chain) {
    const cells = Array.from({ length: 6 }, () => '<div class="param"><div class="skel-line"></div><div class="track"></div></div>').join('');
    el.style.removeProperty('--c');
    el.innerHTML = `<div class="params-head"><div class="skel-title"></div></div><div class="params-grid">${cells}</div>`;
    return;
  }
  const b = slotMap().get(state.selectedSlot);
  const live = canWrite();
  if (!b) {
    const slot = state.selectedSlot;
    el.style.removeProperty('--c');
    if (slot == null) {
      el.innerHTML = '<div class="params-empty">No blocks in this preset.</div>';
      return;
    }
    const add = state.writes
      ? `<button class="act ph-toggle" type="button" data-add${live ? '' : ' disabled'}>Add block</button>`
      : `<button class="later ph-toggle" type="button" disabled title="${esc(WRITES_OFF)}">Add block</button>`;
    el.innerHTML =
      `<div class="params-head"><div class="ph-code">+</div>` +
      `<div class="ph-title"><div class="ph-model">Empty slot</div></div>` +
      `<div class="ph-meta">slot ${slot}</div><div class="spacer"></div>${add}</div>` +
      `<div class="params-empty">${state.writes ? `Add an effect here: tap Add block, pick a model.<br>${esc(footswitches() ? PICK_FOOTSWITCH : NO_FOOTSWITCH)}` : 'Nothing in this slot.'}</div>`;
    return;
  }
  const cat = blockCategory(b);
  el.style.setProperty('--c', cat.colour);
  const kind = cat.subcategory ? `${cat.name} · ${cat.subcategory}` : cat.name;
  const meta = `${kind} · slot ${state.selectedSlot} · ${b.enabled ? 'on' : 'bypassed'}`;
  // The real gear behind the model, from the Pilot's Guide (server: vendor/based-on.json).
  const based = b.based_on ? `<div class="ph-based">Based on ${esc(b.based_on)}</div>` : '';
  const params = b.params.map((p) => {
    const c = controlOf(b, p);
    if (c) return controlHtml(b, p, c, live);
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
  const remove = state.writes
    ? `<button class="act ph-toggle ph-remove" type="button" data-remove${live ? '' : ' disabled'} aria-label="Remove ${esc(b.model)}">Remove</button>`
    : '';
  el.innerHTML =
    `<div class="params-head"><div class="ph-code">${esc(cat.short)}</div>` +
    `<div class="ph-title"><div class="ph-model">${esc(b.model)}</div>${based}</div>` +
    `<div class="ph-meta">${esc(meta)}</div><div class="spacer"></div>` +
    `${remove}${toggle}</div>` +
    footswitchRow(b, live) +
    `<div class="params-grid">${params}</div>`;
}

/** One parameter as a slider, picker or toggle; the value stays tap-to-type. */
function controlHtml(b, p, c, live) {
  const { native, label } = shownValue(b, p);
  const i = indexOf(c, native);
  const off = live ? '' : ' disabled';
  const name = `<span class="param-name">${esc(p.name)}</span>`;
  if (c.kind === 'menu') {
    const options = c.labels.map((l, n) => `<option value="${n}"${n === i ? ' selected' : ''}>${esc(l)}</option>`).join('');
    return `<div class="param ctl"><div class="param-top">${name}</div>` +
      `<select class="menu" data-ctl="${p.index}" data-menu aria-label="${esc(p.name)}"${off}>${options}</select></div>`;
  }
  if (c.kind === 'switch') {
    const on = i === 1;
    return `<div class="param ctl"><div class="param-top">${name}</div>` +
      `<button class="switch" type="button" data-ctl="${p.index}" data-switch aria-pressed="${on}" aria-label="${esc(p.name)}"${off}>` +
      `<span class="switch-knob"></span><span class="switch-label">${esc(c.labels[i])}</span></button></div>`;
  }
  const value = state.writes
    ? `<button class="param-value typed" type="button" data-param="${p.index}"${off} aria-label="${esc(p.name)}: ${esc(label)}. Tap to type a value">${esc(label)}</button>`
    : `<span class="param-value">${esc(label)}</span>`;
  return `<div class="param ctl"><div class="param-top">${name}${value}</div>` +
    `<div class="slider" data-ctl="${p.index}" data-slider role="slider" tabindex="${live ? 0 : -1}" aria-label="${esc(p.name)}"` +
    ` aria-valuemin="0" aria-valuemax="${c.steps}" aria-valuenow="${i}" aria-valuetext="${esc(label)}"` +
    ` aria-disabled="${!live}" style="--pos:${(i / c.steps) * 100}%">` +
    '<div class="track-fill"></div><div class="track-thumb"></div></div></div>';
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
    ? `<span>${esc(deviceName(i))} · FW ${esc(i.firmware)} · BUILD ${esc(i.build)} · ${esc(i.presets)} PRESETS</span>`
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

function renderPicker() {
  const p = state.picker;
  const slot = p.slot;
  $('picker-title').textContent = `Add to slot ${slot}`;
  $('pf-slot').textContent = slot == null ? '' : String(slot);
  const add = $('add-btn');
  const cats = $('cats');
  const models = $('models');

  if (!Array.isArray(state.catalog)) {
    cats.innerHTML = '';
    models.innerHTML = `<div class="no-results">${esc(state.catalog?.error ?? 'Loading models…')}</div>`;
    $('pf-name').textContent = '';
    $('pf-note').textContent = '';
    setControl(add, false);
    return;
  }

  const q = p.filter.trim().toLowerCase();
  cats.innerHTML = state.catalog.map((c, i) =>
    `<button class="cat${!q && i === p.category ? ' active' : ''}" type="button" data-cat="${i}" style="--c:${esc(c.colour)}"` +
    ` aria-pressed="${!q && i === p.category}"><span class="cat-sw"></span><span class="cat-name">${esc(c.name)}</span></button>`).join('');

  // With "All" shelves, a tile says which one it is from.
  const badge = p.shelf === SHELF_ALL;
  const tile = (m) => `<button class="mtile${m.id === p.selected ? ' selected' : ''}" type="button" data-model="${m.id}"` +
    ` style="--c:${esc(m.category.colour)}" aria-pressed="${m.id === p.selected}"` +
    ` aria-label="${esc(m.name)}${m.shelf ? `, ${esc(m.shelf)}` : ''}${m.based_on ? `, based on ${esc(m.based_on)}` : ''}">` +
    `<span class="mtile-top"><span class="mtile-name">${esc(m.name)}</span>` +
    `${badge && m.shelf ? `<span class="mtile-shelf">${esc(m.shelf)}</span>` : ''}</span>` +
    `<span class="mtile-based">${esc(m.based_on ?? '')}</span></button>`;
  const grid = (ms) => `<div class="shelf">${byName(ms).map(tile).join('')}</div>`;

  const all = catalogModels();
  const shown = all.filter((m) => p.shelf === SHELF_ALL || !m.shelf || m.shelf === p.shelf);
  // The shelf filter offers the shelves the visible models come from.
  const pool = q ? all : all.filter((m) => m.ci === p.category);
  const shelfNames = [...new Set(pool.map((m) => m.shelf).filter(Boolean))];
  const shelfBar = shelfNames.length > 1
    ? `<div class="seg" role="group" aria-label="Mono, stereo or legacy">${[SHELF_ALL, ...shelfNames].map((s) =>
      `<button class="seg-btn${s === p.shelf ? ' active' : ''}" type="button" data-shelf="${esc(s)}" aria-pressed="${s === p.shelf}">${esc(s)}</button>`).join('')}</div>`
    : '';

  if (q) {
    // Across every category, grouped by category then type.
    const hits = shown.filter((m) => m.name.toLowerCase().includes(q) ||
      (m.based_on ?? '').toLowerCase().includes(q) || (m.type ?? '').toLowerCase().includes(q));
    const body = hits.length
      ? state.catalog.map((c, ci) => {
        const mine = hits.filter((m) => m.ci === ci);
        if (!mine.length) return '';
        return sections(c, mine).map(([name, ms]) =>
          `<div class="group-name" style="--c:${esc(c.colour)}">${esc(c.name)}${name ? ` · ${esc(name)}` : ''}</div>${grid(ms)}`).join('');
      }).join('')
      : `<div class="no-results">No model matches “${esc(p.filter)}”.</div>`;
    models.innerHTML = (shelfBar ? `<div class="mbar"><div class="spacer"></div>${shelfBar}</div>` : '') + body;
  } else {
    const c = state.catalog[p.category];
    const secs = sections(c, shown.filter((m) => m.ci === p.category));
    // Jump to a type: Boost, Overdrive, Distortion, Fuzz…
    const jumps = c.types?.length
      ? `<div class="tjumps">${secs.map(([name]) =>
        `<button class="tjump" type="button" data-type-jump="${esc(name)}">${esc(name)}</button>`).join('')}</div>`
      : '<div class="spacer"></div>';
    const bar = c.types?.length || shelfBar ? `<div class="mbar">${jumps}${shelfBar}</div>` : '';
    models.innerHTML = bar + (secs.length
      ? secs.map(([name, ms]) =>
        `<section class="msec" data-sec="${esc(name)}">${name ? `<div class="shelf-name">${esc(name)}</div>` : ''}${grid(ms)}</section>`).join('')
      : `<div class="no-results">No ${esc(p.shelf)} models in ${esc(c.name)}.</div>`);
  }

  const sel = all.find((m) => m.id === p.selected);
  $('pf-name').textContent = sel?.name ?? 'Pick a model';
  $('pf-note').textContent = sel
    ? [sel.category.name, sel.type, sel.shelf, sel.based_on && `based on ${sel.based_on}`].filter(Boolean).join(' · ')
    : footswitches() ? PICK_FOOTSWITCH : NO_FOOTSWITCH;
  renderAddFootswitch();
  add.textContent = sel ? `Add to slot ${slot}` : 'Add';
  setControl(add, canWrite() && !!sel && !slotMap().has(slot));
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
    selectSlot(Number(t.dataset.slot));
  } else if (t.matches('[data-add]')) {
    if (state.selectedSlot != null) openPicker(state.selectedSlot);
  } else if (t.matches('[data-remove]')) {
    removeBlock();
  } else if (t.matches('[data-close-picker]')) {
    closePicker();
  } else if (t.matches('[data-cat]')) {
    state.picker.category = Number(t.dataset.cat);
    state.picker.filter = '';
    $('model-filter').value = '';
    renderPicker();
    $('models').scrollTop = 0;
  } else if (t.matches('[data-model]')) {
    // A second tap on the picked model adds it.
    const id = Number(t.dataset.model);
    if (state.picker.selected === id) addBlock();
    else { state.picker.selected = id; renderPicker(); }
  } else if (t.matches('[data-shelf]')) {
    state.picker.shelf = t.dataset.shelf;
    storeShelf(state.picker.shelf);
    renderPicker();
  } else if (t.matches('[data-type-jump]')) {
    const models = $('models');
    const sec = [...models.querySelectorAll('[data-sec]')].find((s) => s.dataset.sec === t.dataset.typeJump);
    const bar = models.querySelector('.mbar');
    if (sec) models.scrollTop = sec.offsetTop - (bar ? bar.offsetHeight : 0);
  } else if (t.id === 'add-btn') {
    addBlock();
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
  } else if (t.matches('[data-fs]')) {
    setFootswitch(Number(t.dataset.fs) || null);
  } else if (t.matches('[data-param]')) {
    editParam(Number(t.dataset.param));
  } else if (t.matches('[data-switch]')) {
    switchToggle(t);
  } else if (t.matches('[data-snapshot]')) {
    selectSnapshot(Number(t.dataset.snapshot));
  } else if (t.id === 'tempo-btn') {
    typeTempo();
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

const paramsEl = $('params');
paramsEl.addEventListener('pointerdown', sliderDown);
paramsEl.addEventListener('pointermove', sliderMove);
paramsEl.addEventListener('pointerup', sliderUp);
paramsEl.addEventListener('pointercancel', sliderUp);
paramsEl.addEventListener('lostpointercapture', sliderUp);
paramsEl.addEventListener('keydown', sliderKey);
paramsEl.addEventListener('change', menuChange);

// Taps count on touch-down, not on release: closer to the beat.
$('tap-btn').addEventListener('pointerdown', (e) => {
  if (e.button !== 0 || $('tap-btn').disabled) return;
  e.preventDefault();
  tap();
});

$('filter').addEventListener('input', (e) => {
  state.browser.filter = e.target.value;
  renderBrowser();
});

$('model-filter').addEventListener('input', (e) => {
  state.picker.filter = e.target.value;
  renderPicker();
});
$('pf-fs').addEventListener('change', (e) => {
  state.picker.fs = e.target.value;
  storeAddFs(e.target.value);
});

$('banks').addEventListener('scroll', updateActiveJump, { passive: true });

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (state.connOpen) { state.connOpen = false; renderConnection(); } else if (state.picker.open) closePicker();
  else if (state.browser.open) closeBrowser();
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

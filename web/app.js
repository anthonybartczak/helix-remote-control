// Helix Bridge UI. No build step; one vendored library (Motion, web/lib) for animation.
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
// Moving: drag a block onto an empty cell (a long press first on touch), or
// tap Move, then the cell.
// Phones show the chain on its own; tapping a block opens its settings full
// screen, Back returns.
// Settings (kept in this browser): Line 6 model names or the gear they are
// based on; the preset list on the left (desktop, tablet landscape).
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

// Names: Line 6's model names, or (a setting) the gear each model is based
// on, from the Pilot's Guide (`based_on`). Line 6's own designs keep their name.
const NAMES_KEY = 'helix-bridge-names';
const LINE6_ORIGINAL = 'Line 6 Original';

function realGear(based) {
  return based && based !== LINE6_ORIGINAL ? based : null;
}

/** A model's name as the names setting shows it. */
function shownName(model, based) {
  return state.names === 'original' ? realGear(based) ?? model : model;
}

/** The line under a name: the other name, so both stay in view. */
function otherName(model, based) {
  if (state.names === 'original' && realGear(based)) return `Line 6 ${model}`;
  return based ? `Based on ${based}` : '';
}

const blockName = (b) => shownName(b.model, b.based_on);

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
  // category: index or RECENT; type: TYPE_ALL or a type filter; selected: a model group's key;
  // variant: Mono | Stereo, the version to add; legacy: show Legacy models. Last two kept in this browser.
  picker: { open: false, slot: null, category: 0, type: 'All', filter: '', selected: null, variant: 'Mono', legacy: false, fs: 'none' },
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
  junction: null, // split | join: shown in the panel instead of a block
  moving: null, // slot of a block being moved (Move, then an empty cell)
  expOpen: null, // position/param whose expression pedal row is open
  travelDrag: null, // heel or toe slider under a finger: { el, index, end, pointer, x0, i0, i, started }
  blockDrag: null, // block picked up in the chain: { slot, pointer, x0, y0, touch, timer, started, ghost, over }
  view: 'chain', // phones: 'chain' (the whole chain) or 'edit' (the selected block, full screen)
  names: 'line6', // 'line6' | 'original' (the gear a model is based on); kept in this browser
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

// Every 30 s, compare the screen with the server's chain and take the
// server's if they differ: a push lost on the way (a socket that died without
// closing, a client too slow to keep up) shows up within half a minute. Live
// backends only: there the server answers from its copy, which it checks
// against the pedal itself (server/src/usb.rs, `audit`), so no USB read.
const CHECK_MS = 30000;

async function checkChain() {
  const quiet = !inflight && !state.busy && !state.drag && !state.travelDrag && !state.blockDrag &&
    !sender.busy() && !tempoSender.busy() && !state.blocked;
  if (!state.live || !state.chain || !quiet || document.visibilityState !== 'visible') return;
  let chain;
  try { chain = await api('/api/chain'); } catch { return; } // a refresh or live message will tell
  if (JSON.stringify(chain) !== JSON.stringify(state.chain) && !state.drag && !state.travelDrag) {
    console.info('Helix Bridge: the screen differed from the server; updated it');
    applyLiveChain(chain);
  }
}

setInterval(checkChain, CHECK_MS);

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

/** The server's live socket is up: it pushes every chain change, writes included. */
function liveOpen() {
  return liveSocket?.readyState === WebSocket.OPEN;
}

/**
 * Send one write. Asks for the token when none is known or it is refused.
 * `then` picks what follows: 'live' (the server pushes the new chain; re-read
 * it only without live updates), 'chain' (re-read the chain), 'full' (also
 * preset names), or a function that applies the result locally with no device
 * read. A read costs 3–5 s with `cli`, so cheap edits use the function.
 * Returns true on success.
 */
async function write(label, path, body, then = 'live', method = 'POST') {
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
  } else if (stale || (ok && (then !== 'live' || !liveOpen()))) {
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
  // Read it, as before: the push comes only once the pedal settles, and until
  // then the old preset would show with nothing saying it is loading.
  return write(`Loading ${p.label}…`, '/api/select', { index }, 'chain');
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
  const value = window.prompt(`${blockName(b)} · ${p.name}\nNow ${p.value}. Type a value in the same units, e.g. ${p.value}:`, p.value);
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

// The picker shows each model once: its Mono and Stereo versions are one tile,
// and the footer picks the version (the last one used, kept in this browser).
// Legacy models (HX Edit's older HD-era shelf) hide behind a toggle; a search
// finds them anyway. "Recent" lists the models added last.
const TYPE_ALL = 'All';
const RECENT = -1; // picker.category for the Recent list
const RECENT_MAX = 8;
const VARIANT_KEY = 'helix-bridge-variant';
const LEGACY_KEY = 'helix-bridge-legacy';
const RECENT_KEY = 'helix-bridge-recent';
const LEGACY = 'Legacy';

function stored(key, fallback) {
  try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; }
}

function store(key, value) {
  try { localStorage.setItem(key, value); } catch { /* private mode: forget it */ }
}

function storedRecent() {
  try {
    const ids = JSON.parse(stored(RECENT_KEY, '[]'));
    return Array.isArray(ids) ? ids.filter(Number.isInteger) : [];
  } catch { return []; }
}

function rememberRecent(id) {
  store(RECENT_KEY, JSON.stringify([id, ...storedRecent().filter((x) => x !== id)].slice(0, RECENT_MAX)));
}

/**
 * A category's groups in sections: by effect type when the server sends
 * types (Boost, Overdrive, Fuzz…; ours, not Line 6's), else one unnamed one.
 * Returns [name, groups] pairs, empty sections left out.
 */
function sections(c, list) {
  if (!c.types?.length) return list.length ? [['', list]] : [];
  const keys = [...c.types, ...new Set(list.map((g) => g.type).filter((t) => !c.types.includes(t)))];
  return keys.map((k) => [k, list.filter((g) => g.type === k)]).filter(([, gs]) => gs.length);
}

let groupCache = { catalog: null, groups: [] };

/**
 * Models merged by name within a category: { key, name, type, based_on,
 * category, ci, legacy, variants: [{ shelf, id }] }. Legacy models stay apart
 * (a few share a name with a current one).
 */
function modelGroups() {
  if (!Array.isArray(state.catalog)) return [];
  if (groupCache.catalog === state.catalog) return groupCache.groups;
  const byKey = new Map();
  for (const m of catalogModels()) {
    const legacy = m.shelf === LEGACY;
    const key = `${m.ci}|${legacy ? 'L' : ''}|${m.name}`;
    if (!byKey.has(key)) {
      byKey.set(key, { key, name: m.name, type: m.type ?? null, based_on: m.based_on ?? null,
        category: m.category, ci: m.ci, legacy, variants: [] });
    }
    byKey.get(key).variants.push({ shelf: m.shelf, id: m.id });
  }
  groupCache = { catalog: state.catalog, groups: [...byKey.values()] };
  return groupCache.groups;
}

/** The model id a group adds: the version picked in the footer, else its only one. */
function variantOf(g) {
  return g.variants.find((v) => v.shelf === state.picker.variant) ?? g.variants[0];
}

function selectedGroup() {
  return modelGroups().find((g) => g.key === state.picker.selected) ?? null;
}

/** Every model in the list, with its category and shelf. */
function catalogModels() {
  if (!Array.isArray(state.catalog)) return [];
  return state.catalog.flatMap((c, ci) => c.shelves.flatMap((s) =>
    s.models.map((m) => ({ ...m, category: c, ci, shelf: s.name ?? null }))));
}

function selectSlot(n) {
  state.selectedSlot = n;
  state.junction = null;
  state.moving = null;
  renderChain();
  renderParams();
  if (!slotMap().has(n) && canWrite()) openPicker(n);
  else openEdit();
}

function openPicker(slot) {
  state.picker = {
    ...state.picker, open: true, slot, filter: '', selected: null, type: TYPE_ALL,
    variant: stored(VARIANT_KEY, 'Mono'), legacy: stored(LEGACY_KEY, '') === '1', fs: storedAddFs(),
  };
  state.connOpen = false;
  $('model-filter').value = '';
  showSheet($('picker'));
  loadCatalog();
  renderConnection();
  renderPicker();
  $('models').scrollTop = 0;
  // Portrait shows categories as a strip: bring the last one used into view.
  document.querySelector('.cat.active')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

/** Redraw the model list from its top, tiles rising in. */
function showModels() {
  renderPicker();
  const models = $('models');
  models.scrollTop = 0;
  fx(models.querySelectorAll('.group-name, .mtile'), { opacity: [0, 1], y: [8, 0] }, { ...RISE, delay: stagger(0.008) });
}

function closePicker() {
  state.picker.open = false;
  hideSheet($('picker'), () => !state.picker.open);
}

async function addBlock() {
  const { slot } = state.picker;
  const g = selectedGroup();
  if (slot == null || !g || slotMap().has(slot) || !canWrite()) return;
  const m = { name: shownName(g.name, g.based_on), id: variantOf(g).id };
  const fs = addFootswitch();
  closePicker();
  state.selectedSlot = slot;
  const added = await write(`Adding ${m.name}…`, `/api/blocks/${slot + 1}`, { model: m.id });
  if (added) rememberRecent(m.id);
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
    .map((p) => { const b = blocks.find((x) => x.position === p); return b ? blockName(b) : `slot ${p - 1}`; });
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
  write(sw ? `Putting ${blockName(b)} on FS${sw}…` : `Taking ${blockName(b)} off FS${b.footswitch}…`,
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
  if (!window.confirm(`Remove ${blockName(b)} from slot ${slotLabel(slot)}?\nThe preset stays unsaved until you save it.`)) return;
  // The model we show: if the pedal now has another one there, the server refuses (409).
  const seen = b.model_id != null ? `?model=${b.model_id}` : '';
  write(`Removing ${blockName(b)}…`, `/api/blocks/${b.position}${seen}`, undefined, 'live', 'DELETE');
}

// ---------- Paths ----------
// Path B runs under Path A between the split and the merge (lanes()). A block
// is added to an empty Path B cell through the picker, like on Path A; the
// first one claims the whole line. Moving a block and moving the split or
// merge rewrite the whole preset on the pedal (TonePush's way): unverified on
// the HX Effects. The rules mirror server/src/routing.rs, which checks them.

/** Whether the split or merge may sit before cell `before` of Path A. */
function junctionOk(which, before) {
  const { main, branch, split, join } = lanes();
  if (!branch.length) return false;
  const [s, j] = which === 'split' ? [before, join] : [split, before];
  if (s < 0 || j > main.length || s >= j) return false;
  const map = slotMap();
  const used = branch.map((n, i) => (map.has(n) ? i : -1)).filter((i) => i >= 0);
  return !used.length || (used[0] >= s && used[used.length - 1] < j);
}

function selectJunction(which) {
  state.junction = which;
  state.moving = null;
  renderChain();
  renderParams();
  openEdit();
}

function moveJunction(delta) {
  const which = state.junction;
  const { split, join } = lanes();
  const before = (which === 'split' ? split : join) + delta;
  if (!which || !junctionOk(which, before)) return;
  write(which === 'split' ? 'Moving the split…' : 'Moving the merge…', '/api/routing', { junction: which, before });
}

/** Tap Move, then an empty cell: the selected block goes there. */
function toggleMove() {
  state.moving = state.moving == null && slotMap().has(state.selectedSlot) ? state.selectedSlot : null;
  renderChain();
  renderParams();
  if (state.moving != null) closeEdit(); // phones: the cells to tap are on the chain view
}

function moveTo(n) {
  const b = slotMap().get(state.moving);
  state.moving = null;
  if (!b) {
    renderChain();
    renderParams();
    return;
  }
  state.selectedSlot = n; // the selection follows the block
  const body = { to: n + 1 };
  if (b.model_id != null) body.model = b.model_id;
  write(`Moving ${blockName(b)} to ${slotLabel(n)}…`, `/api/blocks/${b.position}/move`, body);
}

// Drag and drop: pick a block up and let go over an empty cell, the same
// move as Move then a tap. A mouse picks it up once it travels a few pixels;
// a finger holds still for a moment first, so a swipe still scrolls. While
// one is held, the cells it may go to light up, as for Move.

const HOLD_MS = 320; // touch: hold this long to pick a block up
const HOLD_SLOP_PX = 8; // touch: more travel than this during the hold scrolls instead
const EDGE_PX = 48; // near the chain's top or bottom edge, a held block scrolls it

function movable() {
  return state.writes && !!state.chain?.routing && canWrite();
}

function blockDown(e) {
  const el = e.target.closest('.slot[data-slot]');
  if (!el || state.blockDrag || e.button > 0 || !movable()) return;
  const slot = Number(el.dataset.slot);
  if (!slotMap().has(slot)) return;
  const touch = e.pointerType === 'touch';
  const d = { slot, pointer: e.pointerId, x0: e.clientX, y0: e.clientY, x: e.clientX, y: e.clientY, touch, timer: 0, started: false, ghost: null, over: null };
  if (touch) d.timer = setTimeout(() => startBlockDrag(d), HOLD_MS);
  state.blockDrag = d;
}

function startBlockDrag(d) {
  if (state.blockDrag !== d || !movable() || !slotMap().has(d.slot)) {
    cancelBlockDrag();
    return;
  }
  const chain = $('chain');
  const node = chain.querySelector(`.slot[data-slot="${d.slot}"] .node`);
  d.started = true;
  try { chain.setPointerCapture(d.pointer); } catch { /* pointer already gone */ }
  if (node) {
    const box = node.getBoundingClientRect();
    const ghost = document.createElement('div');
    ghost.className = 'drag-ghost slot on';
    ghost.style.setProperty('--c', getComputedStyle(node).getPropertyValue('--c'));
    ghost.style.setProperty('--node-w', `${box.width}px`);
    ghost.style.setProperty('--node', `${box.height}px`);
    ghost.appendChild(node.cloneNode(true));
    document.body.appendChild(ghost);
    d.ghost = ghost;
    d.dx = d.x0 - box.left;
    d.dy = d.y0 - box.top;
    moveGhost(d);
  }
  if (d.touch) navigator.vibrate?.(12);
  state.moving = d.slot;
  state.selectedSlot = d.slot;
  state.junction = null;
  chain.classList.add('dragging');
  renderChain();
  scrollLoop(d);
}

function moveGhost(d) {
  if (d.ghost) d.ghost.style.transform = `translate(${d.x - d.dx}px, ${d.y - d.dy}px) scale(1.08)`;
}

/** The drop cell under the finger, lit while it is there. */
function dropTarget(d) {
  const under = document.elementFromPoint(d.x, d.y)?.closest('[data-move-to]') ?? null;
  if (under !== d.over) {
    d.over?.classList.remove('over');
    under?.classList.add('over');
    d.over = under;
  }
  return under;
}

/** Scroll the chain while a held block sits near its edge (the phone's tall chain). */
function scrollLoop(d) {
  if (state.blockDrag !== d) return;
  const chain = $('chain');
  const box = chain.getBoundingClientRect();
  const step = d.y < box.top + EDGE_PX ? -8 : d.y > box.bottom - EDGE_PX ? 8 : 0;
  if (step && chain.scrollHeight > chain.clientHeight) {
    chain.scrollTop += step;
    dropTarget(d);
  }
  requestAnimationFrame(() => scrollLoop(d));
}

function blockMove(e) {
  const d = state.blockDrag;
  if (!d || e.pointerId !== d.pointer) return;
  d.x = e.clientX;
  d.y = e.clientY;
  if (!d.started) {
    const far = Math.hypot(d.x - d.x0, d.y - d.y0);
    if (d.touch && far > HOLD_SLOP_PX) cancelBlockDrag(); // a swipe: let it scroll
    else if (!d.touch && far > DRAG_START_PX) startBlockDrag(d);
    return;
  }
  e.preventDefault();
  moveGhost(d);
  dropTarget(d);
}

function blockUp(e) {
  const d = state.blockDrag;
  if (!d || e.pointerId !== d.pointer) return;
  if (!d.started) {
    cancelBlockDrag();
    return;
  }
  d.x = e.clientX || d.x;
  d.y = e.clientY || d.y;
  const target = e.type === 'pointerup' ? dropTarget(d) : null;
  endBlockDrag();
  swallowClick();
  if (target) {
    moveTo(Number(target.dataset.moveTo));
    return;
  }
  state.moving = null; // dropped elsewhere: it stays where it was
  renderChain();
  renderParams();
}

/** Stop a hold or a drag, leaving the chain as it was. */
function cancelBlockDrag() {
  const d = state.blockDrag;
  if (!d) return;
  const started = d.started;
  endBlockDrag();
  if (started) {
    state.moving = null;
    renderChain();
  }
}

function endBlockDrag() {
  const d = state.blockDrag;
  if (!d) return;
  state.blockDrag = null;
  clearTimeout(d.timer);
  d.over?.classList.remove('over');
  d.ghost?.remove();
  const chain = $('chain');
  chain.classList.remove('dragging');
  try { chain.releasePointerCapture(d.pointer); } catch { /* released already */ }
}

// The click that ends a drag lands on whatever is under the mouse: ignore it.
let swallowUntil = 0;
function swallowClick() {
  swallowUntil = performance.now() + 400;
}

// ---------- Phone views ----------
// Under 768 px the chain fills the screen and the selected block's panel
// opens over it. The browser's Back (a history entry) closes it too.

const phone = window.matchMedia('(max-width: 767px)');

function openEdit() {
  if (!phone.matches || state.view === 'edit') return;
  state.view = 'edit';
  try { history.pushState({ helixEdit: true }, ''); } catch { /* sandboxed: Back button only */ }
  renderView();
  $('params').scrollTop = 0;
}

function closeEdit() {
  if (state.view !== 'edit') return;
  if (history.state?.helixEdit) {
    history.back(); // popstate shows the chain
    return;
  }
  state.view = 'chain';
  renderView();
}

window.addEventListener('popstate', () => {
  if (state.view === 'edit' && !history.state?.helixEdit) {
    state.view = 'chain';
    renderView();
  }
});

function renderView() {
  const app = $('app');
  const before = app.classList.contains('view-edit');
  const edit = state.view === 'edit';
  app.classList.toggle('view-edit', edit);
  if (before === edit || !phone.matches) return;
  if (edit) fx($('params'), { opacity: [0, 1], x: [36, 0] }, { ...SPRING, opacity: FADE });
  else fx($('chain'), { opacity: [0, 1], x: [-36, 0] }, { ...SPRING, opacity: FADE });
}

/** Start Path B: pick a model for its first cell; it parallels the whole line. */
function addPathB() {
  const first = lanes().cells[0];
  if (first == null) return;
  state.selectedSlot = first;
  state.junction = null;
  openPicker(first);
}

// ---------- Expression pedals ----------
// A setting can follow expression pedal 1 or 2 between a heel and a toe end
// (/api/blocks/{p}/params/{i}/controller). Its EXP tag opens a row: pick the
// pedal, then set the heel or toe to the slider's current value. The device
// table also holds footswitch, MIDI and snapshot controllers; their sources
// are unconfirmed on the HX Effects, so they show as CTL n, read only.
// Writes are unverified on the HX Effects.

/** A block's assignment for parameter `index`, or for its on/off with null. */
function assignmentOf(b, index) {
  return (b?.assignments ?? []).find((a) => (a.param ?? null) === index) ?? null;
}

function sourceLabel(source) {
  const m = /^exp(\d)$/.exec(source);
  return m ? `EXP ${m[1]}` : `CTL ${source.replace(/^ctl/, '')}`;
}

/** Pedal assignments need the preset document (usb, fake), as moves do. */
function pedalsEditable() {
  return state.writes && !!state.chain?.routing;
}

/** A setting's EXP tag (a button with writes) and, when open, its pedal row. */
function pedalParts(b, p, c, live) {
  const a = assignmentOf(b, p.index);
  if (!a && !pedalsEditable()) return { tag: '', row: '' };
  const key = `${b.position}/${p.index}`;
  const open = state.expOpen === key;
  const label = a ? sourceLabel(a.source) : 'EXP';
  const tag = pedalsEditable()
    ? `<button class="exp-tag${a ? ' on' : ''}${open ? ' open' : ''}" type="button" data-exp="${p.index}"` +
      ` aria-expanded="${open}" aria-label="${esc(p.name)}: expression pedal, ${a ? label : 'none'}">${label}</button>`
    : `<span class="exp-tag on">${label}</span>`;
  const travel = a && a.min != null
    ? `<div class="exp-travel">Heel ${esc(a.min_value ?? a.min)} → Toe ${esc(a.max_value ?? a.max)}</div>`
    : '';
  if (!open) return { tag, row: travel };
  const off = live ? '' : ' disabled';
  const on = a?.source ?? null;
  const seg = [0, 1, 2].map((n) => {
    const active = n ? on === `exp${n}` : on == null;
    return `<button class="seg-btn${active ? ' active' : ''}" type="button" data-exp-set="${n}" aria-pressed="${active}"${off}>${n ? `EXP ${n}` : 'None'}</button>`;
  }).join('');
  const other = on && !/^exp/.test(on) ? `<div class="exp-note">Now on ${sourceLabel(on)} (a footswitch, MIDI or snapshots); picking a pedal replaces it.</div>` : '';
  // Heel and toe: small sliders on the setting's own grid, sent on release.
  const travelSlider = (end, native, shown) => {
    const i = indexOf(c, native);
    const name = end === 'min' ? 'Heel' : 'Toe';
    return `<div class="travel"><div class="travel-top"><span class="travel-name">${name}</span>` +
      `<span class="travel-value">${esc(shown ?? c.labels[i])}</span></div>` +
      `<div class="slider slider-sm" data-travel="${end}" role="slider" tabindex="${live ? 0 : -1}"` +
      ` aria-label="${esc(p.name)}, ${name.toLowerCase()} end" aria-valuemin="0" aria-valuemax="${c.steps}"` +
      ` aria-valuenow="${i}" aria-valuetext="${esc(shown ?? c.labels[i])}" aria-disabled="${!live}"` +
      ` style="--pos:${(i / c.steps) * 100}%"><div class="track-fill"></div><div class="track-thumb"></div></div></div>`;
  };
  const ends = a && a.min != null && c.kind === 'continuous'
    ? `<div class="travels">${travelSlider('min', a.min, a.min_value)}${travelSlider('max', a.max, a.max_value)}</div>` +
      `<button class="act exp-end" type="button" data-exp-end="full"${off}>Full range</button>` +
      '<div class="exp-note">Heel: the pedal pulled back. Toe: pushed down.</div>'
    : '';
  return { tag, row: `${travel}<div class="exp-row" data-exp-row="${p.index}"><div class="seg">${seg}</div>${other}${ends}</div>` };
}

function toggleExp(index) {
  const b = slotMap().get(state.selectedSlot);
  if (!b) return;
  const key = `${b.position}/${index}`;
  state.expOpen = state.expOpen === key ? null : key;
  renderParams();
}

function setPedal(index, pedal) {
  const b = slotMap().get(state.selectedSlot);
  const p = b?.params.find((x) => x.index === index);
  if (!p) return;
  const body = { pedal: pedal || null };
  if (b.model_id != null) body.model = b.model_id;
  write(pedal ? `Putting ${p.name} on EXP ${pedal}…` : `Taking ${p.name} off its pedal…`,
    `/api/blocks/${b.position}/params/${index}/controller`, body);
}

/** The selected block's setting `index` and its control, if it has one. */
function travelTarget(index) {
  const b = slotMap().get(state.selectedSlot);
  const p = b?.params.find((x) => x.index === index);
  const c = p && controlOf(b, p);
  return c ? { b, p, c } : null;
}

/** Send a heel (`min`), toe (`max`) or `full` range; `native` for heel or toe. */
function setTravel(index, end, native) {
  const t = travelTarget(index);
  if (!t) return;
  const { b, p, c } = t;
  const body = end === 'full' ? { min: c.min, max: c.max } : { [end]: native };
  if (b.model_id != null) body.model = b.model_id;
  const what = { min: 'heel', max: 'toe', full: 'travel' }[end];
  write(`Setting ${p.name}'s ${what}…`, `/api/blocks/${b.position}/params/${index}/controller/range`, body);
}

// Dragging a heel or toe slider: relative to where the finger lands, as the
// setting sliders; a vertical swipe still scrolls. One write on release.

function travelDown(e) {
  const el = e.target.closest('[data-travel]');
  if (!el || state.travelDrag || state.drag || el.getAttribute('aria-disabled') === 'true') return;
  const index = Number(el.closest('[data-exp-row]').dataset.expRow);
  if (!travelTarget(index)) return;
  const i = Number(el.getAttribute('aria-valuenow'));
  state.travelDrag = { el, index, end: el.dataset.travel, pointer: e.pointerId, x0: e.clientX, i0: i, i, started: false };
  try { el.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ }
}

function paintTravel(el, c, i) {
  el.style.setProperty('--pos', `${(i / c.steps) * 100}%`);
  el.setAttribute('aria-valuenow', String(i));
  el.setAttribute('aria-valuetext', c.labels[i]);
  const value = el.parentElement.querySelector('.travel-value');
  if (value) value.textContent = c.labels[i];
}

function travelMove(e) {
  const d = state.travelDrag;
  if (!d || e.pointerId !== d.pointer) return;
  const t = travelTarget(d.index);
  if (!t) return;
  const dx = e.clientX - d.x0;
  if (!d.started) {
    if (Math.abs(dx) < DRAG_START_PX) return;
    d.started = true;
    d.el.classList.add('dragging');
  }
  const width = d.el.clientWidth || 1;
  const i = Math.max(0, Math.min(t.c.steps, Math.round(d.i0 + (dx / width) * t.c.steps)));
  if (i === d.i) return;
  d.i = i;
  paintTravel(d.el, t.c, i);
}

function travelUp(e) {
  const d = state.travelDrag;
  if (!d || e.pointerId !== d.pointer) return;
  state.travelDrag = null;
  d.el.classList.remove('dragging');
  try { d.el.releasePointerCapture(d.pointer); } catch { /* released already */ }
  const t = travelTarget(d.index);
  if (t && d.started && d.i !== d.i0) setTravel(d.index, d.end, nativeAt(t.c, d.i));
  else renderParams();
}

function travelKey(e) {
  const el = e.target.closest('[data-travel]');
  const step = { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1 }[e.key];
  if (!el || !step || el.getAttribute('aria-disabled') === 'true') return;
  e.preventDefault();
  const index = Number(el.closest('[data-exp-row]').dataset.expRow);
  const t = travelTarget(index);
  if (!t) return;
  const i = Math.max(0, Math.min(t.c.steps, Number(el.getAttribute('aria-valuenow')) + step));
  paintTravel(el, t.c, i);
  setTravel(index, el.dataset.travel, nativeAt(t.c, i));
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
    liveOpen() ? () => { state.chain.snapshot = index; } : 'chain');
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
  if (!samePreset) closeEdit(); // phones: another preset starts on its chain
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

// ---------- Motion ----------
// Springs from Motion (web/lib/motion-*.js, motion.dev, MIT). Each render
// rebuilds its elements, so CSS transitions never see a change; the renders
// call these when they see one. Without Motion, or with Reduce Motion on, the
// UI is the same, only still.

const M = window.Motion ?? null;
const calm = window.matchMedia('(prefers-reduced-motion: reduce)');
const SPRING = { type: 'spring', bounce: 0.3, visualDuration: 0.35 };
const FADE = { duration: 0.2, ease: 'easeOut' };
const RISE = { duration: 0.28, ease: [0.2, 0.8, 0.2, 1] };

/** Animate `els`, then hand transform and opacity back to the stylesheet. */
function fx(els, keyframes, options) {
  const list = els instanceof Element ? [els] : [...(els || [])];
  if (!M || calm.matches || !list.length) return Promise.resolve();
  return M.animate(list, keyframes, options).then(() => {
    for (const el of list) {
      el.style.removeProperty('transform');
      el.style.removeProperty('opacity');
    }
  });
}

const stagger = (s) => (M ? M.stagger(s) : 0);

function showSheet(el) {
  el.hidden = false;
  fx(el, { opacity: [0, 1], y: [28, 0] }, { ...SPRING, opacity: FADE });
}

/** Slide a sheet away; `stillClosed` guards against a reopen mid-way. */
function hideSheet(el, stillClosed) {
  if (!M || calm.matches) {
    el.hidden = true;
    return;
  }
  M.animate(el, { opacity: 0, y: 20 }, { duration: 0.16, ease: 'easeIn' }).then(() => {
    el.style.removeProperty('transform');
    el.style.removeProperty('opacity');
    if (stillClosed()) el.hidden = true;
  });
}

// What the last render showed, to animate only what changed.
const seen = { label: undefined, slot: null, blocks: new Map(), params: null, barLabel: undefined, snap: null, banner: false };

/** Chain: a new preset deals its blocks in; an added, removed or switched block pops. */
function chainFx(map) {
  const label = state.chain ? state.chain.label : null;
  const blocks = new Map([...map].map(([n, b]) => [n, `${b.model}/${b.enabled}`]));
  const before = { ...seen };
  seen.label = label;
  seen.slot = state.selectedSlot;
  seen.blocks = blocks;
  if (label == null) return;
  const chain = $('chain');
  if (label !== before.label) {
    fx(chain.querySelectorAll('.slot'), { opacity: [0, 1], y: [14, 0], scale: [0.86, 1] },
      { ...SPRING, opacity: FADE, delay: stagger(0.03) });
    return;
  }
  const at = (n) => chain.querySelector(`.slot[data-slot="${n}"]`);
  let moved = false;
  for (const n of new Set([...before.blocks.keys(), ...blocks.keys()])) {
    const was = before.blocks.get(n);
    const now = blocks.get(n);
    if (was === now) continue;
    moved ||= n === state.selectedSlot;
    if (now && !was) fx(at(n), { scale: [0.5, 1], opacity: [0, 1] }, { ...SPRING, bounce: 0.45, opacity: FADE });
    else if (now) fx(at(n), { scale: [0.88, 1.06, 1] }, { duration: 0.35, ease: 'easeOut' });
    else fx(at(n), { opacity: [0, 1], scale: [0.7, 1] }, SPRING);
  }
  if (!moved && state.selectedSlot !== before.slot) {
    fx(at(state.selectedSlot), { scale: [0.9, 1] }, { ...SPRING, bounce: 0.5 });
  }
}

/** Parameters: another block (or preset) rises in, row by row. */
function paramsFx(el, key) {
  if (key === seen.params) return;
  seen.params = key;
  if (key == null) return;
  fx(el.querySelectorAll('.params-head, .fs-row, .param, .params-empty'), { opacity: [0, 1], y: [10, 0] },
    { ...RISE, delay: stagger(0.012) });
}

/** Preset bar: a new name slides in; a new snapshot springs. */
function presetBarFx(label, snap) {
  const before = { label: seen.barLabel, snap: seen.snap };
  seen.barLabel = label;
  seen.snap = snap;
  if (label == null) return;
  if (label !== before.label) {
    fx([$('nb-label'), $('nb-name')], { opacity: [0, 1], x: [-10, 0] }, { ...RISE, delay: stagger(0.04) });
  } else if (snap !== before.snap && snap != null) {
    fx(document.querySelectorAll('.snap.active'), { scale: [0.86, 1] }, { ...SPRING, bounce: 0.5 });
  }
}

// ---------- Rendering ----------

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function render() {
  const app = $('app');
  app.classList.toggle('is-loading', state.loading);
  app.classList.toggle('is-blocked', !!state.blocked);
  $('blocker').hidden = !state.blocked;
  renderView();
  renderConnection();
  renderPresetBar();
  renderBanner();
  renderChain();
  renderParams();
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
      (name ? `<span class="conn-name">${esc(name)}</span>` : '') +
      '<svg class="conn-gear" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2"><use href="#gear"/></svg>';
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
  renderSettings();
  $('conn-popover').hidden = !state.connOpen;
}

/** The settings in the popover: names, preset list. */
function renderSettings() {
  const seg = (attr, value, label, on) => `<button class="seg-btn${on ? ' active' : ''}" type="button" ${attr}="${value}" aria-pressed="${on}">${label}</button>`;
  $('set-names').innerHTML = seg('data-names', 'line6', 'Line 6', state.names !== 'original') +
    seg('data-names', 'original', 'Original gear', state.names === 'original');
  const shown = sidebarWanted();
  $('set-sidebar').innerHTML = seg('data-sidebar-set', 'shown', 'Show', shown) + seg('data-sidebar-set', 'hidden', 'Hide', !shown);
  $('set-sidebar-note').textContent = roomy.matches
    ? 'Tap a preset in the list to load it.'
    : 'Needs a wider screen: a tablet in landscape or a computer. Here the Presets button lists them.';
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
  presetBarFx(state.chain?.label ?? null, active ?? null);
}

function renderBanner() {
  const b = state.banner;
  const shown = !!b && !state.blocked;
  $('banner').hidden = !shown;
  if (shown && !seen.banner) fx($('banner'), { opacity: [0, 1], y: [-10, 0] }, { ...SPRING, opacity: FADE });
  seen.banner = shown;
  if (!b) return;
  $('banner-msg').textContent = b.message;
  $('banner-msg').title = b.message;
  $('banner-ctx').textContent = b.context;
}

/**
 * Path A and Path B from /api/chain .routing, as slot numbers (position - 1).
 * `branch` lists Path B only while it is in use (a block on it, the split and
 * merge attached); `cells` lists Path B's cells always. `split` and `join`
 * are cells of Path A the fork and the merge sit just before. Without routing
 * (`cli`), one row of SLOTS and no Path B.
 */
function lanes() {
  const r = state.chain?.routing;
  const main = r?.main?.length ? r.main.map((p) => p - 1) : Array.from({ length: SLOTS }, (_, i) => i + 1);
  const cells = (r?.branch ?? []).map((p) => p - 1);
  const split = r?.split?.before;
  const join = r?.join?.before;
  const divides = cells.length > 0 && split != null && join != null && split < join;
  return { main, cells, branch: divides ? cells : [], split, join };
}

/**
 * A block drawn as a pedal: an expression pedal for wahs and volume, else a
 * stompbox. Its colour is the category's (--c on the slot).
 */
function pedal(cat) {
  const plate = `<span class="plate">${esc(cat.short)}</span>`;
  if (/^(wah|volume)/i.test(cat.name)) {
    return `<div class="node treadle"><span class="led"></span><span class="tread"></span>${plate}</div>`;
  }
  // An FX loop: a jack box with two plugs in its top edge, Send and Return,
  // their cables running straight up out of it.
  if (/^send/i.test(cat.name)) {
    return '<div class="node loop"><svg class="cables" viewBox="0 0 74 30" aria-hidden="true">' +
      '<path class="cable" d="M22 15V-2"/><path class="cable" d="M52 15V-2"/>' +
      '<rect class="plug" x="18.5" y="14" width="7" height="11" rx="1.5"/>' +
      '<rect class="plug" x="48.5" y="14" width="7" height="11" rx="1.5"/></svg>' +
      '<span class="led"></span><span class="jacks"><span>SEND</span><span>RTN</span></span>' +
      `${plate}</div>`;
  }
  return `<div class="node"><span class="led"></span>${plate}</div>`;
}

/** A slot as the UI names it: 1-9 on Path A, B1-B9 on Path B. */
function slotLabel(n) {
  const i = lanes().cells.indexOf(n);
  return i >= 0 ? `B${i + 1}` : String(n);
}

/**
 * Empty cells a block may move into (server/src/routing.rs): Path A's, and
 * Path B's under the split and merge, or all of Path B's while it is empty
 * (the first block there claims the whole line).
 */
function moveTargets() {
  const { main, cells, branch, split, join } = lanes();
  const map = slotMap();
  const onB = branch.length ? cells.filter((_, i) => i >= split && i < join) : cells;
  return new Set([...main, ...onB].filter((n) => !map.has(n)));
}

function renderChain() {
  const skeleton = !state.chain;
  const map = slotMap();
  const { main, cells, branch, split, join } = lanes();
  const moving = state.moving != null && map.has(state.moving) ? state.moving : null;
  const targets = moving == null ? null : moveTargets();
  const cell = (n, label) => {
    if (skeleton) {
      return '<div class="slot"><div class="slot-num"></div><div class="node-skel"></div><div class="slot-name"></div></div>';
    }
    const b = map.get(n);
    const sel = n === state.selectedSlot && !state.junction;
    if (!b) {
      if (targets?.has(n)) {
        return `<button class="slot empty target" type="button" data-move-to="${n}" aria-label="Move ${esc(blockName(map.get(moving)))} to slot ${label}">` +
          `<div class="slot-num">${label}</div><div class="node-empty"></div><div class="slot-name"></div></button>`;
      }
      return `<button class="slot empty${sel ? ' selected' : ''}" type="button" data-slot="${n}" aria-pressed="${sel}"` +
        ` aria-label="Slot ${label}: empty${state.writes ? '. Tap to add a block' : ''}">` +
        `<div class="slot-num">${label}</div><div class="node-empty"></div><div class="slot-name"></div></button>`;
    }
    const cat = blockCategory(b);
    const cls = ['slot', b.enabled ? 'on' : '', sel ? 'selected' : '', n === moving ? 'moving' : ''].join(' ');
    const exp = [...new Set((b.assignments ?? []).map((a) => sourceLabel(a.source).replace(' ', '')))];
    const tags = (b.footswitch ? [`<span class="slot-fs">FS${b.footswitch}</span>`] : [])
      .concat(exp.map((e) => `<span class="slot-fs slot-exp">${e}</span>`));
    const name = blockName(b);
    // The phone's vertical chain has room for a second line: tags and the other
    // name, or the category for Line 6's own designs.
    const other = realGear(b.based_on) ? otherName(b.model, b.based_on) : cat.name;
    const sub = `<div class="slot-sub">${tags.join(' ')} ${esc(other)}</div>`;
    return `<button class="${cls}" type="button" data-slot="${n}" style="--c:${cat.colour}"` +
      ` aria-pressed="${sel}" aria-label="Slot ${label}: ${esc(name)}, ${esc(cat.name)}${b.based_on ? `, based on ${esc(b.based_on)}` : ''}${b.enabled ? '' : ', bypassed'}${b.footswitch ? `, on footswitch ${b.footswitch}` : ''}"${b.based_on ? ` title="${esc(b.model)}: ${esc(b.based_on)}"` : ''}>` +
      `<div class="slot-num">${label}<span class="slot-tags">${tags.map((t) => ` · ${t}`).join('')}</span></div>${pedal(cat)}` +
      `<div class="slot-name">${esc(name)}</div>${sub}</button>`;
  };
  // While a block moves and Path B is empty, Path B shows as a drop row.
  const ghost = !branch.length && moving != null && cells.length > 0;
  const rowB = branch.length ? branch : ghost ? cells : [];
  let html = '<div class="io"><div class="io-bar"></div>In</div>';
  // Phones: Move left the block's panel, so say here what a tap does now.
  if (moving != null && !state.blockDrag?.started) {
    html += `<div class="move-hint"><span>Tap an empty cell for ${esc(blockName(map.get(moving)))}</span>` +
      '<button class="act ph-toggle" type="button" data-move>Cancel</button></div>';
  }
  if (rowB.length) {
    // Two rows on one grid: Path B's cell k under Path A's cell k, as HX Edit
    // draws it. The bracket runs from the split down to Path B and back up to
    // the merge; Path B's cells outside it carry no signal and stay blank.
    const [from, to] = ghost ? [0, main.length] : [split, join];
    const junction = (which, at) => {
      const name = which === 'split' ? 'Split' : 'Merge';
      const sel = state.junction === which;
      return `<button class="junction${sel ? ' selected' : ''}" type="button" data-junction="${which}" style="--at:${at}"` +
        ` aria-pressed="${sel}" aria-label="${name}, before cell ${at + 1}"></button>`;
    };
    html += `<div class="slots two-rows" style="--cols:${main.length};--from:${from};--to:${to}">` +
      `<div class="lane-b${ghost ? ' ghost' : ''}" aria-hidden="true"><span class="lane-tag">B</span></div>` +
      main.map((n) => cell(n, String(n))).join('') +
      rowB.map((n, i) => (i < from || i >= to) && !map.has(n)
        ? '<div class="slot off" aria-hidden="true"></div>'
        : cell(n, `B${i + 1}`)).join('') +
      (ghost ? '' : junction('split', split) + junction('join', join)) +
      '</div>';
  } else {
    html += `<div class="slots">${main.map((n) => cell(n, String(n))).join('')}</div>`;
  }
  html += '<div class="io">Out</div>';
  // Path B empty and writes on: a way to start it.
  if (!skeleton && cells.length && !rowB.length && state.writes) {
    html += `<button class="add-b" type="button" data-add-b${canWrite() ? '' : ' disabled'}>+ Path B</button>`;
  }
  const chain = $('chain');
  chain.classList.toggle('split', rowB.length > 0);
  chain.classList.toggle('has-add-b', html.includes('data-add-b'));
  chain.innerHTML = html;
  chainFx(map);
}

// Phones only (CSS): back from the full-screen panel to the chain.
const BACK = '<button class="icon-btn back-btn" type="button" data-back aria-label="Back to the chain">' +
  '<svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 5l-7 7 7 7"/></svg></button>';

function renderParams() {
  // Never rebuild a slider under the finger; sliderUp / travelUp render.
  if (state.drag || state.travelDrag) return;
  paintParams();
  const b = state.chain && slotMap().get(state.selectedSlot);
  const shown = state.junction ?? `${state.selectedSlot}/${b?.model ?? ''}`;
  paramsFx($('params'), state.chain ? `${state.chain.label}/${shown}` : null);
}

/** The split or merge in the panel: where it sits, and buttons to move it. */
function paintJunction(el) {
  const which = state.junction;
  const { main, split, join } = lanes();
  const at = which === 'split' ? split : join;
  const name = which === 'split' ? 'Split' : 'Merge';
  const live = canWrite();
  const step = (d, label) => state.writes
    ? `<button class="act ph-toggle" type="button" data-junction-step="${d}"${live && junctionOk(which, at + d) ? '' : ' disabled'}>${label}</button>`
    : `<button class="later ph-toggle" type="button" disabled title="${esc(WRITES_OFF)}">${label}</button>`;
  el.style.setProperty('--c', 'var(--line-strong)');
  el.innerHTML =
    `<div class="params-head">${BACK}<div class="ph-code">${which === 'split' ? 'Split' : 'Merge'}</div>` +
    `<div class="ph-title"><div class="ph-model">${name}</div>` +
    `<div class="ph-based">before ${at >= main.length ? 'the output' : `cell ${at + 1}`}</div></div>` +
    `<div class="ph-meta">Path B runs under cells ${split + 1} to ${join}</div><div class="spacer"></div>` +
    `${step(-1, '‹ Earlier')}${step(1, 'Later ›')}</div>` +
    '<div class="params-empty">Blocks on Path A before the split and after the merge carry the whole signal; ' +
    'between them Path A and Path B run side by side. Path B’s blocks stay between the two.<br>' +
    'Moving the split or merge rewrites the whole preset on the pedal (edit buffer, unsaved until you save). ' +
    'Split type and merge levels are not editable here yet.</div>';
}

function paintParams() {
  const el = $('params');
  if (state.chain && state.junction && lanes().branch.length) {
    paintJunction(el);
    return;
  }
  state.junction = null;
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
      `<div class="params-head">${BACK}<div class="ph-code">+</div>` +
      `<div class="ph-title"><div class="ph-model">Empty slot</div></div>` +
      `<div class="ph-meta">slot ${slotLabel(slot)}</div><div class="spacer"></div>${add}</div>` +
      `<div class="params-empty">${state.writes ? `Add an effect here: tap Add block, pick a model.<br>${esc(footswitches() ? PICK_FOOTSWITCH : NO_FOOTSWITCH)}` : 'Nothing in this slot.'}</div>`;
    return;
  }
  const cat = blockCategory(b);
  el.style.setProperty('--c', cat.colour);
  const kind = cat.subcategory ? `${cat.name} · ${cat.subcategory}` : cat.name;
  const onOff = assignmentOf(b, null);
  const meta = `${kind} · slot ${slotLabel(state.selectedSlot)} · ${b.enabled ? 'on' : 'bypassed'}` +
    (onOff ? ` · ${sourceLabel(onOff.source)} switches it` : '');
  // The real gear behind the model, from the Pilot's Guide (server: vendor/based-on.json),
  // or the Line 6 name when the names setting shows the gear.
  const other = otherName(b.model, b.based_on);
  const based = other ? `<div class="ph-based">${esc(other)}</div>` : '';
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
    ? `<button class="act ph-toggle ph-remove" type="button" data-remove${live ? '' : ' disabled'} aria-label="Remove ${esc(blockName(b))}">Remove</button>`
    : '';
  // Moving needs the preset document (routing): usb and fake.
  const moving = state.moving === state.selectedSlot;
  const move = state.writes && state.chain.routing
    ? `<button class="act ph-toggle${moving ? ' hot' : ''}" type="button" data-move${live ? '' : ' disabled'}` +
      ` aria-pressed="${moving}">${moving ? 'Cancel move' : 'Move'}</button>`
    : '';
  el.innerHTML =
    `<div class="params-head">${BACK}<div class="ph-code">${esc(cat.short)}</div>` +
    `<div class="ph-title"><div class="ph-model">${esc(blockName(b))}</div>${based}</div>` +
    `<div class="ph-meta">${esc(moving ? 'Tap an empty cell to move it there' : meta)}</div><div class="spacer"></div>` +
    `${move}${remove}${toggle}</div>` +
    footswitchRow(b, live) +
    `<div class="params-grid">${params}</div>`;
}

/** One parameter as a slider, picker or toggle; the value stays tap-to-type. */
function controlHtml(b, p, c, live) {
  const { native, label } = shownValue(b, p);
  const i = indexOf(c, native);
  const off = live ? '' : ' disabled';
  const pedal = pedalParts(b, p, c, live);
  const name = `<span class="param-label"><span class="param-name">${esc(p.name)}</span>${pedal.tag}</span>`;
  if (c.kind === 'menu') {
    const options = c.labels.map((l, n) => `<option value="${n}"${n === i ? ' selected' : ''}>${esc(l)}</option>`).join('');
    return `<div class="param ctl"><div class="param-top">${name}</div>` +
      `<select class="menu" data-ctl="${p.index}" data-menu aria-label="${esc(p.name)}"${off}>${options}</select>${pedal.row}</div>`;
  }
  if (c.kind === 'switch') {
    const on = i === 1;
    return `<div class="param ctl"><div class="param-top">${name}</div>` +
      `<button class="switch" type="button" data-ctl="${p.index}" data-switch aria-pressed="${on}" aria-label="${esc(p.name)}"${off}>` +
      `<span class="switch-knob"></span><span class="switch-label">${esc(c.labels[i])}</span></button>${pedal.row}</div>`;
  }
  const value = state.writes
    ? `<button class="param-value typed" type="button" data-param="${p.index}"${off} aria-label="${esc(p.name)}: ${esc(label)}. Tap to type a value">${esc(label)}</button>`
    : `<span class="param-value">${esc(label)}</span>`;
  return `<div class="param ctl"><div class="param-top">${name}${value}</div>` +
    `<div class="slider" data-ctl="${p.index}" data-slider role="slider" tabindex="${live ? 0 : -1}" aria-label="${esc(p.name)}"` +
    ` aria-valuemin="0" aria-valuemax="${c.steps}" aria-valuenow="${i}" aria-valuetext="${esc(label)}"` +
    ` aria-disabled="${!live}" style="--pos:${(i / c.steps) * 100}%">` +
    `<div class="track-fill"></div><div class="track-thumb"></div></div>${pedal.row}</div>`;
}

let listKey = '';
function renderPresetList() {
  const key = state.presets.map((p) => `${p.label}${p.name}${p.loaded}`).join('|');
  if (key === listKey) return;
  listKey = key;
  const list = $('preset-list');
  // A tap loads, as in the Presets sheet; unsaved edits ask first (loadPreset).
  list.innerHTML = state.presets.map((p) =>
    `<li class="${p.label.endsWith('A') && p.index > 0 ? 'bank-start' : ''}"><button class="pl-row${p.loaded ? ' loaded' : ''}" type="button"` +
    ` data-pl="${p.index}" aria-current="${p.loaded}" aria-label="${esc(p.label)} ${esc(p.name)}${p.loaded ? ', loaded' : '. Tap to load'}">` +
    `<span class="pl-label">${esc(p.label)}</span><span class="pl-name">${esc(p.name)}</span></button></li>`).join('');
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
  $('picker-title').textContent = `Add to slot ${slotLabel(slot)}`;
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
  const groups = modelGroups();
  const recentIds = storedRecent();
  const recent = recentIds.map((id) => groups.find((g) => g.variants.some((v) => v.id === id))).filter(Boolean)
    .filter((g, i, list) => list.indexOf(g) === i);
  if (p.category === RECENT && !recent.length) p.category = 0;
  const catBtn = (i, name, colour, extra = '') =>
    `<button class="cat${!q && i === p.category ? ' active' : ''}${extra}" type="button" data-cat="${i}" style="--c:${esc(colour)}"` +
    ` aria-pressed="${!q && i === p.category}"><span class="cat-sw"></span><span class="cat-name">${esc(name)}</span></button>`;
  cats.innerHTML = (recent.length ? catBtn(RECENT, 'Recent', 'var(--ink)', ' cat-recent') : '') +
    state.catalog.map((c, i) => catBtn(i, c.name, c.colour)).join('');

  // A badge only where it tells something: Legacy, or a model with one version.
  const badge = (g) => g.legacy ? LEGACY : g.variants.length === 1 && g.variants[0].shelf ? g.variants[0].shelf : '';
  // With the names setting on the gear, the gear is the tile's name and Line 6's goes under it.
  const gear = (g) => state.names === 'original' && !!realGear(g.based_on);
  const tile = (g) => {
    const sel = g.key === p.selected;
    const b = badge(g);
    const name = gear(g) ? g.based_on : g.name;
    const sub = gear(g) ? `Line 6 ${g.name}` : g.based_on ?? '';
    return `<button class="mtile${sel ? ' selected' : ''}" type="button" data-group="${esc(g.key)}"` +
      ` style="--c:${esc(g.category.colour)}" aria-pressed="${sel}"` +
      ` aria-label="${esc(name)}${b ? `, ${esc(b)} only` : ''}${gear(g) ? `, Line 6 ${esc(g.name)}` : g.based_on ? `, based on ${esc(g.based_on)}` : ''}">` +
      `<span class="mtile-top"><span class="mtile-name">${esc(name)}</span>${b ? `<span class="mtile-shelf">${esc(b)}</span>` : ''}</span>` +
      `<span class="mtile-based">${esc(sub)}</span></button>`;
  };
  const grid = (gs) => `<div class="shelf">${gs.map(tile).join('')}</div>`;
  const head = (name, colour) => `<div class="group-name"${colour ? ` style="--c:${esc(colour)}"` : ''}>${esc(name)}</div>`;

  let body;
  let bar = '';
  if (q) {
    // Across every category, Legacy included: a search means a specific model.
    const hits = groups.filter((g) => g.name.toLowerCase().includes(q) ||
      (g.based_on ?? '').toLowerCase().includes(q) || (g.type ?? '').toLowerCase().includes(q));
    body = hits.length
      ? state.catalog.map((c, ci) => sections(c, hits.filter((g) => g.ci === ci)).map(([name, gs]) =>
        head(`${c.name}${name ? ` · ${name}` : ''}`, c.colour) + grid(gs)).join('')).join('')
      : `<div class="no-results">No model matches “${esc(p.filter)}”.</div>`;
  } else if (p.category === RECENT) {
    body = head('Added last') + grid(recent);
  } else {
    const c = state.catalog[p.category];
    const mine = groups.filter((g) => g.ci === p.category);
    const legacyCount = mine.filter((g) => g.legacy).length;
    const pool = mine.filter((g) => p.legacy || !g.legacy);
    const secs = sections(c, pool);
    if (!secs.some(([name]) => name === p.type)) p.type = TYPE_ALL;
    // Type chips filter: one type at a time, or all of them in sections.
    const chip = (name, n) => `<button class="tchip${name === p.type ? ' active' : ''}" type="button" data-type="${esc(name)}"` +
      ` aria-pressed="${name === p.type}">${esc(name)}<span class="tchip-n">${n}</span></button>`;
    const chips = c.types?.length && secs.length > 1
      ? `<div class="tjumps" role="group" aria-label="Type">${chip(TYPE_ALL, pool.length)}${secs.map(([name, gs]) => chip(name, gs.length)).join('')}</div>`
      : '<div class="spacer"></div>';
    const legacy = legacyCount
      ? `<button class="tchip legacy-toggle${p.legacy ? ' active' : ''}" type="button" data-legacy aria-pressed="${p.legacy}"` +
        ` title="HX Edit's older (HD-era) models">Legacy<span class="tchip-n">${legacyCount}</span></button>`
      : '';
    bar = (chips + legacy) === '<div class="spacer"></div>' ? '' : `<div class="mbar">${chips}${legacy}</div>`;
    const shown = p.type === TYPE_ALL ? secs : secs.filter(([name]) => name === p.type);
    body = shown.length
      ? shown.map(([name, gs]) => (name && shown.length > 1 ? head(name) : '') + grid(gs)).join('')
      : `<div class="no-results">No models in ${esc(c.name)}.</div>`;
  }
  models.innerHTML = bar + body;

  const g = selectedGroup();
  const v = g && variantOf(g);
  $('pf-name').textContent = g ? shownName(g.name, g.based_on) : 'Pick a model';
  $('pf-note').textContent = g
    ? [g.category.name, g.type, g.legacy && LEGACY, gear(g) ? `Line 6 ${g.name}` : g.based_on && `based on ${g.based_on}`].filter(Boolean).join(' · ')
    : footswitches() ? PICK_FOOTSWITCH : NO_FOOTSWITCH;
  // Mono or Stereo, for a model that has both.
  const variants = $('pf-variant');
  variants.hidden = !g || g.variants.length < 2;
  variants.innerHTML = variants.hidden ? '' : g.variants.map((x) =>
    `<button class="seg-btn${x === v ? ' active' : ''}" type="button" data-variant="${esc(x.shelf)}" aria-pressed="${x === v}">${esc(x.shelf)}</button>`).join('');
  renderAddFootswitch();
  add.textContent = g ? `Add to slot ${slotLabel(slot)}` : 'Add';
  setControl(add, canWrite() && !!g && !slotMap().has(slot));
}

function openBrowser() {
  state.browser.open = true;
  state.browser.selected = state.browser.selected ?? loadedPreset()?.index ?? null;
  state.connOpen = false;
  showSheet($('browser'));
  renderConnection();
  renderBrowser();
  const sel = $('banks').querySelector('.selected') || $('banks').querySelector('.loaded');
  if (sel) sel.scrollIntoView({ block: 'center' });
  updateActiveJump();
}

function closeBrowser() {
  state.browser.open = false;
  hideSheet($('browser'), () => !state.browser.open);
}

// ---------- Events ----------

// ---------- Preset list (sidebar) ----------
// Shown or hidden from the top bar or the settings, kept in this browser:
// 'shown' or 'hidden'; unset (or '' from before), computers show it and
// tablets in landscape do not. Too narrow a screen never shows it. Hidden,
// the Presets button opens the full sheet instead.

const SIDEBAR_KEY = 'helix-bridge-sidebar';
const desktop = window.matchMedia('(min-width: 1280px)');
const roomy = window.matchMedia('(min-width: 1280px), (min-width: 1024px) and (orientation: landscape)');

function sidebarSetting() {
  const v = stored(SIDEBAR_KEY, '');
  return v === 'shown' || v === 'hidden' ? v : '';
}

function sidebarWanted() {
  const v = sidebarSetting();
  return v ? v === 'shown' : desktop.matches;
}

function setSidebar(animate) {
  const v = sidebarSetting();
  const app = $('app');
  app.classList.toggle('no-sidebar', v === 'hidden');
  app.classList.toggle('sidebar-on', v === 'shown');
  const open = sidebarWanted() && roomy.matches;
  const btn = document.querySelector('[data-sidebar]');
  btn.setAttribute('aria-pressed', String(open));
  btn.title = open ? 'Hide the preset list' : 'Show the preset list';
  if (!open || !animate) return;
  fx($('sidebar'), { opacity: [0, 1], x: [-24, 0] }, { ...SPRING, opacity: FADE });
  $('preset-list').querySelector('.loaded')?.scrollIntoView({ block: 'center' });
}

function showSidebar(open) {
  store(SIDEBAR_KEY, open ? 'shown' : 'hidden');
  setSidebar(true);
  renderSettings();
}

function toggleSidebar() {
  showSidebar(!sidebarWanted());
}

function setNames(names) {
  state.names = names === 'original' ? 'original' : 'line6';
  store(NAMES_KEY, state.names);
  render();
}

// A drag that ends over a button must not also tap it (blockUp).
document.addEventListener('click', (e) => {
  if (performance.now() < swallowUntil) {
    swallowUntil = 0;
    e.stopPropagation();
    e.preventDefault();
  }
}, true);

document.addEventListener('click', (e) => {
  const t = e.target.closest('button, [data-conn]');
  if (state.connOpen && !e.target.closest('#conn-popover, [data-conn]')) {
    state.connOpen = false;
    renderConnection();
  }
  if (!t) return;
  if (t.matches('[data-sidebar]')) {
    toggleSidebar();
  } else if (t.matches('[data-sidebar-set]')) {
    showSidebar(t.dataset.sidebarSet === 'shown');
  } else if (t.matches('[data-names]')) {
    setNames(t.dataset.names);
  } else if (t.matches('[data-back]')) {
    closeEdit();
  } else if (t.matches('[data-pl]')) {
    loadPreset(Number(t.dataset.pl));
  } else if (t.matches('[data-conn]')) {
    state.connOpen = !state.connOpen;
    renderConnection();
    if (state.connOpen) fx($('conn-popover'), { opacity: [0, 1], scale: [0.94, 1], y: [-8, 0] }, { ...SPRING, opacity: FADE });
  } else if (t.matches('[data-retry]')) {
    state.connOpen = false;
    state.banner = null;
    refresh();
  } else if (t.id === 'banner-close') {
    state.banner = null;
    renderBanner();
  } else if (t.matches('[data-move-to]')) {
    moveTo(Number(t.dataset.moveTo));
  } else if (t.matches('[data-slot]')) {
    selectSlot(Number(t.dataset.slot));
  } else if (t.matches('[data-exp]')) {
    toggleExp(Number(t.dataset.exp));
  } else if (t.matches('[data-exp-set]')) {
    setPedal(Number(t.closest('[data-exp-row]').dataset.expRow), Number(t.dataset.expSet));
  } else if (t.matches('[data-exp-end]')) {
    setTravel(Number(t.closest('[data-exp-row]').dataset.expRow), 'full');
  } else if (t.matches('[data-move]')) {
    toggleMove();
  } else if (t.matches('[data-junction]')) {
    selectJunction(t.dataset.junction);
  } else if (t.matches('[data-junction-step]')) {
    moveJunction(Number(t.dataset.junctionStep));
  } else if (t.matches('[data-add-b]')) {
    addPathB();
  } else if (t.matches('[data-add]')) {
    if (state.selectedSlot != null) openPicker(state.selectedSlot);
  } else if (t.matches('[data-remove]')) {
    removeBlock();
  } else if (t.matches('[data-close-picker]')) {
    closePicker();
  } else if (t.matches('[data-cat]')) {
    Object.assign(state.picker, { category: Number(t.dataset.cat), type: TYPE_ALL, filter: '' });
    $('model-filter').value = '';
    showModels();
  } else if (t.matches('[data-type]')) {
    state.picker.type = t.dataset.type;
    showModels();
  } else if (t.matches('[data-legacy]')) {
    state.picker.legacy = !state.picker.legacy;
    store(LEGACY_KEY, state.picker.legacy ? '1' : '');
    showModels();
  } else if (t.matches('[data-group]')) {
    // A second tap on the picked model adds it.
    const key = t.dataset.group;
    if (state.picker.selected === key) addBlock();
    else { state.picker.selected = key; renderPicker(); }
  } else if (t.matches('[data-variant]')) {
    state.picker.variant = t.dataset.variant;
    store(VARIANT_KEY, state.picker.variant);
    renderPicker();
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
paramsEl.addEventListener('pointerdown', travelDown);
paramsEl.addEventListener('pointermove', travelMove);
paramsEl.addEventListener('pointerup', travelUp);
paramsEl.addEventListener('pointercancel', travelUp);
paramsEl.addEventListener('lostpointercapture', travelUp);
paramsEl.addEventListener('keydown', travelKey);
paramsEl.addEventListener('change', menuChange);

const chainEl = $('chain');
chainEl.addEventListener('pointerdown', blockDown);
chainEl.addEventListener('pointermove', blockMove);
chainEl.addEventListener('pointerup', blockUp);
chainEl.addEventListener('pointercancel', blockUp);
// Once a block is held, a moving finger drags it instead of scrolling the page.
chainEl.addEventListener('touchmove', (e) => { if (state.blockDrag?.started) e.preventDefault(); }, { passive: false });
chainEl.addEventListener('contextmenu', (e) => { if (state.blockDrag) e.preventDefault(); });

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

// At most once a frame: each update measures every bank row.
let jumpFrame = 0;
$('banks').addEventListener('scroll', () => {
  if (!jumpFrame) jumpFrame = requestAnimationFrame(() => { jumpFrame = 0; updateActiveJump(); });
}, { passive: true });

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (state.blockDrag) cancelBlockDrag();
  else if (state.connOpen) { state.connOpen = false; renderConnection(); } else if (state.picker.open) closePicker();
  else if (state.moving != null) toggleMove();
  else if (state.browser.open) closeBrowser();
  else closeEdit();
});

// Re-read when the iPad comes back to the page (screen lock, app switch).
// No polling with `cli`: each read is a USB session on the pedal (checkChain
// runs only with live updates).
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible') return;
  if (Date.now() - lastRefresh > RETRY_MS) refresh({ full: false });
  connectLive();
});

state.names = stored(NAMES_KEY, 'line6') === 'original' ? 'original' : 'line6';
setSidebar(false);
// Rotating or resizing across a breakpoint: the sidebar and the phone views follow.
for (const m of [desktop, roomy]) m.addEventListener('change', () => { setSidebar(false); renderSettings(); });
phone.addEventListener('change', () => { if (!phone.matches) closeEdit(); render(); });
render();
refresh();

// Playwright check: drag a parameter slider against the fake backend.
//
//   cargo build --manifest-path server/Cargo.toml
//   NODE_PATH=$(npm root -g) node web/tests/drag.mjs
//
// Starts server/target/debug/helix-bridge (HELIX_BIN overrides) on a free
// port with HELIX_BACKEND=fake and a 40 ms reply delay, drives Chromium in
// iPad landscape and portrait sizes, and checks: controls are >= 44 px tall,
// at most one write is in flight, writes start >= 75 ms apart, the slider
// never jumps back under the finger, and the server ends on the value shown.
// Also: a snapshot tap lights it and the server agrees; four TAPs 400 ms
// apart send one tempo write of about 150 BPM.
// SHOTS=dir saves screenshots.
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import assert from 'node:assert/strict';

const { chromium } = createRequire(import.meta.url)('playwright');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const bin = process.env.HELIX_BIN || path.join(root, 'server/target/debug/helix-bridge');
const port = 18000 + Math.floor(Math.random() * 1000);
const base = `http://127.0.0.1:${port}`;
const TOKEN = 'drag-test-token';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = spawn(bin, [], {
  env: { ...process.env, HELIX_BACKEND: 'fake', HELIX_TOKEN: TOKEN, HELIX_BIND: `127.0.0.1:${port}`, HELIX_FAKE_DELAY_MS: '40', RUST_LOG: 'warn' },
  stdio: ['ignore', 'inherit', 'inherit'],
});

async function waitForServer() {
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(`${base}/api/health`)).ok) return; } catch { /* not yet */ }
    await sleep(100);
  }
  throw new Error('server did not start');
}

/** Watch slider writes: start times and the most in flight at once. */
function watchWrites(page) {
  const w = { starts: [], bodies: [], urls: [], inflight: 0, maxInflight: 0 };
  const isWrite = (r) => r.method() === 'POST' && /\/api\/blocks\/\d+\/params\/\d+$/.test(r.url());
  page.on('request', (r) => {
    if (r.method() === 'POST') w.urls.push(r.url());
    if (!isWrite(r)) return;
    w.starts.push(Date.now());
    w.bodies.push(JSON.parse(r.postData()));
    w.inflight += 1;
    w.maxInflight = Math.max(w.maxInflight, w.inflight);
  });
  const done = (r) => { if (isWrite(r)) w.inflight -= 1; };
  page.on('requestfinished', done);
  page.on('requestfailed', done);
  return w;
}

async function settle(w) {
  for (let i = 0; i < 100 && w.inflight > 0; i++) await sleep(50);
  await sleep(400);
}

async function chainJson() {
  return (await fetch(`${base}/api/chain`)).json();
}

async function waitFor(check, message, ms = 5000) {
  for (const end = Date.now() + ms; Date.now() < end; await sleep(100)) {
    if (await check()) return;
  }
  throw new Error(message);
}

async function chainParam(position, index) {
  const chain = await (await fetch(`${base}/api/chain`)).json();
  return chain.blocks.find((b) => b.position === position).params[index];
}

async function drag(page, slider, fraction, steps = 25) {
  const box = await slider.boundingBox();
  const y = box.y + box.height / 2;
  const x0 = box.x + box.width * 0.2;
  const seen = [];
  await page.mouse.move(x0, y);
  await page.mouse.down();
  for (let s = 1; s <= steps; s++) {
    await page.mouse.move(x0 + (box.width * fraction * s) / steps, y);
    seen.push(Number(await slider.getAttribute('aria-valuenow')));
    await sleep(30);
  }
  await page.mouse.up();
  return seen;
}

async function run(browser, name, viewport) {
  const context = await browser.newContext({ viewport, deviceScaleFactor: 1 });
  await context.addInitScript((t) => localStorage.setItem('helix-bridge-token', t), TOKEN);
  const page = await context.newPage();
  page.on('dialog', (d) => { throw new Error(`unexpected dialog: ${d.message()}`); });
  const w = watchWrites(page);
  await page.goto(base);

  // Slot 1 is Fassel (chain position 2); Position is parameter 0.
  const slider = page.locator('[data-slider][data-ctl="0"]');
  await slider.waitFor();
  await sleep(800); // presets and device info arrive and re-render
  for (const el of [slider, page.locator('.param-value.typed').first()]) {
    await el.waitFor();
    const box = await el.boundingBox();
    assert.ok(box.height >= 44, `${name}: control ${box.height}px tall`);
  }

  const seen = await drag(page, slider, 0.6);
  await settle(w);
  // The slider follows the finger and never jumps back during the drag.
  for (let i = 1; i < seen.length; i++) assert.ok(seen[i] >= seen[i - 1], `${name}: jumped back ${seen}`);
  assert.ok(w.starts.length >= 2, `${name}: ${w.starts.length} writes`);
  assert.ok(w.starts.length < seen.length, `${name}: one write per move step (${w.starts.length})`);
  assert.equal(w.maxInflight, 1, `${name}: ${w.maxInflight} writes in flight at once`);
  for (let i = 1; i < w.starts.length; i++) {
    assert.ok(w.starts[i] - w.starts[i - 1] >= 75, `${name}: writes ${w.starts[i] - w.starts[i - 1]} ms apart`);
  }
  const shown = await page.locator('.param-value.typed').first().textContent();
  const last = w.bodies.at(-1);
  const server = await chainParam(2, 0);
  assert.equal(last.model, 269);
  assert.ok(Math.abs(server.native - last.native) < 1e-6, `${name}: server ${server.native}, sent ${last.native}`);
  assert.equal(server.value, shown, `${name}: server shows ${server.value}, UI ${shown}`);
  assert.equal(await slider.getAttribute('aria-valuenow'), String(seen.at(-1)));
  if (process.env.SHOTS) await page.screenshot({ path: path.join(process.env.SHOTS, `${name}-slider.png`) });

  // A menu (Simple Pitch Interval, synthetic) and a switch (FX Loop param 3).
  await page.locator('[data-slot="2"]').click();
  const menu = page.locator('[data-menu][data-ctl="0"]');
  assert.ok((await menu.boundingBox()).height >= 44);
  await menu.selectOption({ label: '+12' });
  await settle(w);
  assert.equal((await chainParam(3, 0)).value, '+12');
  assert.equal(w.bodies.at(-1).native, 12);

  await page.locator('[data-slot="4"]').click();
  const toggle = page.locator('[data-switch][data-ctl="3"]');
  assert.ok((await toggle.boundingBox()).height >= 44);
  const before = await toggle.getAttribute('aria-pressed');
  await toggle.click();
  await settle(w);
  assert.notEqual(await toggle.getAttribute('aria-pressed'), before);
  assert.equal((await chainParam(5, 3)).value, before === 'true' ? 'Off' : 'On');
  if (process.env.SHOTS) await page.screenshot({ path: path.join(process.env.SHOTS, `${name}-switch.png`) });

  // Snapshots: one not active yet lights up and the server agrees.
  const target = (((await chainJson()).snapshot ?? 0) + 1) % 4;
  const snap = page.locator(`[data-snapshot="${target}"]:visible`);
  assert.ok((await snap.boundingBox()).height >= 44, `${name}: snapshot button too small`);
  await snap.click();
  await waitFor(async () => (await chainJson()).snapshot === target, `${name}: snapshot ${target + 1} not active`);
  await waitFor(async () => (await snap.getAttribute('aria-pressed')) === 'true', `${name}: snapshot ${target + 1} not lit`);

  // Tap tempo: four taps 400 ms apart are 150 BPM; one write once they pause.
  const tapBtn = page.locator('#tap-btn');
  const tapBox = await tapBtn.boundingBox();
  assert.ok(tapBox.height >= 44, `${name}: TAP ${tapBox.height}px tall`);
  const tempoWrites = () => w.urls.filter((u) => u.endsWith('/api/tempo')).length;
  const tempoBefore = tempoWrites();
  for (let i = 0; i < 4; i++) {
    await page.mouse.move(tapBox.x + tapBox.width / 2, tapBox.y + tapBox.height / 2);
    await page.mouse.down();
    await page.mouse.up();
    if (i < 3) await sleep(400);
  }
  await waitFor(async () => tempoWrites() > tempoBefore, `${name}: no tempo write`);
  await settle(w);
  assert.equal(tempoWrites() - tempoBefore, 1, `${name}: one tempo write per tap series`);
  const tempo = (await chainJson()).tempo;
  assert.ok(tempo >= 140 && tempo <= 160, `${name}: tapped ${tempo} BPM, expected about 150`);
  assert.equal(Number(await page.locator('#tempo').textContent()), tempo);

  console.log(`ok ${name}: ${w.starts.length} writes for ${seen.length} move steps, max 1 in flight; snapshot ${target + 1}; tapped ${tempo} BPM`);
  await context.close();
}

let failed = false;
try {
  await waitForServer();
  const browser = await chromium.launch();
  try {
    await run(browser, 'ipad-landscape', { width: 1180, height: 820 });
    await run(browser, 'ipad-portrait', { width: 820, height: 1180 });
  } finally {
    await browser.close();
  }
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  server.kill();
}
process.exit(failed ? 1 : 0);

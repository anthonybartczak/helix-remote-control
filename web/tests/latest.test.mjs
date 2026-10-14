// Unit tests for web/latest.js. Run: node --test web/tests/
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const { LatestSender } = createRequire(import.meta.url)('../latest.js');

/** A fake clock and timer queue, plus a send whose replies the test releases. */
function rig(gap = 80) {
  let now = 0;
  let timers = [];
  const sent = [];
  const replies = [];
  const idle = [];
  const s = new LatestSender((key, value) => {
    sent.push([now, key, value]);
    return new Promise((resolve, reject) => replies.push({ resolve, reject }));
  }, {
    gap,
    now: () => now,
    setTimer: (fn, ms) => { const t = { at: now + ms, fn }; timers.push(t); return t; },
    clearTimer: (t) => { timers = timers.filter((x) => x !== t); },
    onIdle: (key) => idle.push(key),
  });
  const flush = () => new Promise((r) => setImmediate(r));
  return {
    s, sent, idle,
    async advance(ms) {
      now += ms;
      for (const t of timers.filter((x) => x.at <= now)) { timers = timers.filter((x) => x !== t); t.fn(); }
      await flush();
    },
    async reply(ok = true) {
      const r = replies.shift();
      if (ok) r.resolve('ok'); else r.reject(new Error('no'));
      await flush();
    },
  };
}

test('a drag sends the first value at once and only the last one after it', async () => {
  const r = rig();
  r.s.push('2/0', 0.1);
  await r.advance(0);
  for (const v of [0.2, 0.3, 0.4, 0.5]) r.s.push('2/0', v);
  await r.advance(10);
  assert.deepEqual(r.sent.map((x) => x[2]), [0.1]); // one in flight
  await r.reply();
  await r.advance(100);
  assert.deepEqual(r.sent.map((x) => x[2]), [0.1, 0.5]); // last value wins
  await r.reply();
  assert.equal(r.s.busy(), false);
  assert.deepEqual(r.idle, ['2/0']);
});

test('request starts stay at least gap ms apart', async () => {
  const r = rig(80);
  r.s.push('k', 1);
  await r.advance(0);
  await r.reply(); // fast reply at t=0
  r.s.push('k', 2);
  await r.advance(30);
  assert.equal(r.sent.length, 1); // still inside the gap
  await r.advance(50);
  assert.equal(r.sent.length, 2);
  assert.ok(r.sent[1][0] - r.sent[0][0] >= 80);
});

test('two parameters each keep their newest value, one request at a time', async () => {
  const r = rig();
  r.s.push('a', 1);
  r.s.push('b', 1);
  r.s.push('a', 2);
  r.s.push('b', 2);
  await r.advance(0);
  assert.equal(r.sent.length, 1);
  await r.reply();
  await r.advance(100);
  await r.reply();
  await r.advance(100);
  await r.reply();
  assert.deepEqual(r.sent.map((x) => `${x[1]}=${x[2]}`), ['a=1', 'b=2', 'a=2']);
});

test('cancel drops what waits after a failed write', async () => {
  const r = rig();
  const errors = [];
  r.s.onError = (key) => { errors.push(key); r.s.cancel(key); };
  r.s.push('k', 1);
  await r.advance(0);
  r.s.push('k', 2);
  await r.reply(false);
  await r.advance(200);
  assert.deepEqual(errors, ['k']);
  assert.equal(r.sent.length, 1);
  assert.equal(r.s.busy('k'), false);
});

// LatestSender: rate-limited writes for sliders. No build step, no dependencies.
//
// At most one request in flight, request starts at least `gap` ms apart, and
// for each key only the newest value waits: a value pushed while another for
// the same key waits replaces it (last value wins). Nothing queues up behind a
// drag, so the pedal never gets a burst (PROTOCOL.md: it wedges when hammered).
// Different keys (two parameters) each keep their newest value, sent in turn.
'use strict';

class LatestSender {
  /**
   * @param {(key: string, value: any) => Promise<any>} send one request
   * @param {object} [opts]
   * @param {number} [opts.gap] least ms between request starts
   * @param {(key, value, result) => void} [opts.onResult]
   * @param {(key, value, error) => void} [opts.onError]
   * @param {(key) => void} [opts.onIdle] nothing in flight or waiting for `key`
   * @param {() => number} [opts.now] clock, for tests
   * @param {(fn, ms) => any} [opts.setTimer] for tests
   * @param {(id) => void} [opts.clearTimer] for tests
   */
  constructor(send, opts = {}) {
    this.send = send;
    this.gap = opts.gap ?? 80;
    this.onResult = opts.onResult || (() => {});
    this.onError = opts.onError || (() => {});
    this.onIdle = opts.onIdle || (() => {});
    this.now = opts.now || (() => Date.now());
    this.setTimer = opts.setTimer || ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = opts.clearTimer || ((id) => clearTimeout(id));
    this.pending = new Map(); // key -> newest value not yet sent
    this.inflight = null; // key being sent
    this.lastStart = -Infinity;
    this.timer = null;
  }

  /** Send `value` for `key` as soon as allowed, replacing any waiting one. */
  push(key, value) {
    this.pending.set(key, value);
    this._pump();
  }

  /** Drop what waits for `key` (e.g. after a failed write). */
  cancel(key) {
    this.pending.delete(key);
    if (!this.pending.size && this.timer != null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    this._idleCheck(key);
  }

  /** True while `key` (or, without a key, anything) is in flight or waiting. */
  busy(key) {
    if (key === undefined) return this.inflight != null || this.pending.size > 0;
    return this.inflight === key || this.pending.has(key);
  }

  _pump() {
    if (this.inflight != null || this.timer != null || !this.pending.size) return;
    const wait = this.lastStart + this.gap - this.now();
    if (wait > 0) {
      this.timer = this.setTimer(() => { this.timer = null; this._pump(); }, wait);
      return;
    }
    const [key, value] = this.pending.entries().next().value;
    this.pending.delete(key);
    this.inflight = key;
    this.lastStart = this.now();
    Promise.resolve()
      .then(() => this.send(key, value))
      .then((result) => this.onResult(key, value, result), (error) => this.onError(key, value, error))
      .finally(() => {
        this.inflight = null;
        this._pump();
        this._idleCheck(key);
      });
  }

  _idleCheck(key) {
    if (!this.busy(key)) this.onIdle(key);
  }
}

if (typeof module === 'object' && module.exports) module.exports = { LatestSender };

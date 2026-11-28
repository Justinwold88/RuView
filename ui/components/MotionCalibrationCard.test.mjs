import { test } from 'node:test';
import assert from 'node:assert/strict';

const store = new Map();
globalThis.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};

const {
  MotionCalibrationController,
  nodeRows,
  nodeDetailText,
  requestErrorText,
  runPhaseText,
  START_PATH,
  STATUS_PATH,
} = await import('./MotionCalibrationCard.js');
const { API_TOKEN_STORAGE_KEY } = await import('../services/api.service.js');

// Shapes below follow motion_calibration_status / motion_calibration_start in
// v2/crates/wifi-densepose-sensing-server/src/main.rs.
const calibratedNode = {
  presence_threshold: 0.031,
  calibrated: true,
  calibration: {
    presence_threshold: 0.031,
    noisy: false,
    empty_p50: 0.011,
    empty_p99: 0.024,
    empty_max: 0.03,
    frames: 1200,
    seconds: 60,
    link_quality: { score: 62, snr_db: 25.4, frame_rate_hz: 20.1, loss_pct: 0.5, noise_ratio: 0.6 },
  },
};

function response(status, body) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

/** A fake fetch that answers from a queue of [path, status, body]. */
function fakeFetch(queue) {
  const calls = [];
  const fn = async (path, init) => {
    calls.push({ path, init });
    const next = queue.shift();
    assert.ok(next, `unexpected request ${init.method} ${path}`);
    assert.equal(path, next[0]);
    if (next[1] === 'throw') throw new TypeError('Failed to fetch');
    return response(next[1], next[2]);
  };
  fn.calls = calls;
  return fn;
}

/** Timers that never fire on their own; `fire()` runs the pending one. */
function manualTimers() {
  let pending = null;
  return {
    setTimer: (fn) => { pending = fn; return 1; },
    clearTimer: () => { pending = null; },
    async fire() {
      const fn = pending;
      pending = null;
      assert.ok(fn, 'no poll was scheduled');
      await fn();
    },
    get scheduled() { return pending !== null; },
  };
}

test('nodeRows merges current nodes with the last run and sorts by node id', () => {
  const rows = nodeRows({
    nodes: {
      10: calibratedNode,
      2: { presence_threshold: 0.04, calibrated: false, calibration: null },
    },
    last_run: {
      2: { success: false, error: 'only 12 frames arrived; at least 200 are needed' },
      10: { success: true, calibration: calibratedNode.calibration },
    },
  });
  assert.deepEqual(rows.map((r) => r.nodeId), [2, 10]);
  assert.equal(rows[0].calibrated, false);
  assert.equal(rows[0].linkScore, null);
  assert.match(nodeDetailText(rows[0]), /Last run failed: only 12 frames/);
  assert.equal(rows[1].linkScore, 62);
  assert.equal(rows[1].threshold, 0.031);
  assert.equal(nodeDetailText(rows[1]), 'SNR 25 dB · 20 frames/s · 0.5% lost');
});

test('a noisy link says only clear motion registers; an uncalibrated one says so', () => {
  const noisy = nodeRows({ nodes: { 1: { ...calibratedNode, calibration: { ...calibratedNode.calibration, noisy: true } } } });
  assert.match(nodeDetailText(noisy[0]), /noisy: only clear motion registers/);
  const fresh = nodeRows({ nodes: { 1: { presence_threshold: 0.04, calibrated: false, calibration: null } } });
  assert.equal(nodeDetailText(fresh[0]), 'Not calibrated: using the default threshold.');
  assert.deepEqual(nodeRows(null), []);
});

test('run phases read as instructions', () => {
  // seconds_remaining counts to the end of the run, waiting included: 9.2 s of
  // waiting is left when 69.2 s remain of a 60 s collection.
  assert.equal(runPhaseText({ phase: 'waiting', seconds_remaining: 69.2, frames: {} }, 60), 'Leave the room now. Recording starts in 10 s.');
  assert.equal(runPhaseText({ phase: 'waiting', seconds_remaining: 69.2, frames: {} }), 'Leave the room now. The run ends in 70 s.');
  // frames is a per-node map; the text sums it.
  assert.equal(runPhaseText({ phase: 'collecting', seconds_remaining: 41, frames: { 1: 300, 2: 80 } }), 'Keep the room empty: 41 s left (380 frames so far).');
  assert.equal(runPhaseText({ phase: 'due', seconds_remaining: 0, frames: { 1: 1200 } }), 'Finishing…');
  assert.equal(runPhaseText(null), '');
});

test('auth failures point at the API token instead of a bare status code', () => {
  assert.match(requestErrorText(401, null), /needs an API token/);
  assert.match(requestErrorText(403, { error: 'insufficient_scope' }), /RUVIEW_API_TOKEN/);
  assert.match(requestErrorText(0, null), /Could not reach/);
  assert.equal(requestErrorText(400, { success: false, error: 'duration out of range' }), 'duration out of range');
  assert.equal(requestErrorText(500, null), 'The server answered HTTP 500.');
});

test('start → waiting → collecting → finished, polling only while a run is in progress', async () => {
  store.set(API_TOKEN_STORAGE_KEY, 'tok-123');
  const timers = manualTimers();
  const fetchImpl = fakeFetch([
    [START_PATH, 202, { success: true, phase: 'waiting', delay_secs: 10, duration_secs: 60, instructions: 'Leave the room within 10 s and keep it empty for 60 s.' }],
    [STATUS_PATH, 200, { run: { phase: 'waiting', seconds_remaining: 70, frames: {} }, last_run: null, nodes: {} }],
    [STATUS_PATH, 200, { run: { phase: 'collecting', seconds_remaining: 59, frames: { 1: 20 } }, last_run: null, nodes: {} }],
    [STATUS_PATH, 200, { run: null, last_run: { 1: { success: true, calibration: calibratedNode.calibration } }, nodes: { 1: calibratedNode } }],
  ]);
  const seen = [];
  const c = new MotionCalibrationController({ fetchImpl, ...timers, onChange: (s) => seen.push(s) });

  await c.start();
  assert.equal(fetchImpl.calls[0].init.method, 'POST');
  assert.equal(fetchImpl.calls[0].init.headers.Authorization, 'Bearer tok-123');
  assert.equal(fetchImpl.calls[0].init.body, undefined, 'the server picks the default delay and duration');
  assert.equal(c.state.running, true);
  assert.equal(c.state.message, 'Leave the room now. Recording starts in 10 s.');
  assert.ok(seen.some((s) => s.busy), 'the button is disabled while the request is in flight');
  assert.ok(timers.scheduled);

  await timers.fire();
  assert.equal(c.state.message, 'Keep the room empty: 59 s left (20 frames so far).');

  await timers.fire();
  assert.equal(c.state.running, false);
  assert.equal(c.state.message, 'Calibration finished.');
  assert.equal(nodeRows(c.state.status)[0].linkScore, 62);
  assert.equal(timers.scheduled, false, 'no polling once the run is over');
  store.clear();
});

test('a run started elsewhere (409) is shown instead of reported as an error', async () => {
  const timers = manualTimers();
  const fetchImpl = fakeFetch([
    [START_PATH, 409, { success: false, error_code: 'motion_calibration_running', error: 'A motion calibration is already running.' }],
    [STATUS_PATH, 200, { run: { phase: 'collecting', seconds_remaining: 30, frames: { 1: 600 } }, last_run: null, nodes: {} }],
  ]);
  const c = new MotionCalibrationController({ fetchImpl, ...timers });
  await c.start();
  assert.equal(c.state.error, null);
  assert.equal(c.state.running, true);
  assert.ok(timers.scheduled);
  assert.equal(fetchImpl.calls[0].init.headers.Authorization, undefined, 'no token stored, no header');
});

test('a rejected start shows why and does not poll', async () => {
  const timers = manualTimers();
  const fetchImpl = fakeFetch([[START_PATH, 403, { error: 'insufficient_scope' }]]);
  const c = new MotionCalibrationController({ fetchImpl, ...timers });
  await c.start();
  assert.match(c.state.error, /RUVIEW_API_TOKEN/);
  assert.equal(c.state.busy, false);
  assert.equal(c.state.running, false);
  assert.equal(timers.scheduled, false);
});

test('an unreachable server is an error, and stop() cancels polling', async () => {
  const timers = manualTimers();
  const fetchImpl = fakeFetch([
    [STATUS_PATH, 'throw'],
    [STATUS_PATH, 200, { run: { phase: 'waiting', seconds_remaining: 65, frames: {} }, last_run: null, nodes: {} }],
  ]);
  const c = new MotionCalibrationController({ fetchImpl, ...timers });
  await c.refresh();
  assert.match(c.state.error, /Could not reach/);
  await c.refresh();
  assert.ok(timers.scheduled);
  c.stop();
  assert.equal(timers.scheduled, false);
});

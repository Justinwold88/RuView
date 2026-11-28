/**
 * MotionCalibrationCard — run the empty-room calibration from the Sensing tab
 * (issue #52, plan item 1.1).
 *
 * The server side is `POST /api/v1/calibration/motion/start` and
 * `GET /api/v1/calibration/motion` (`motion_calibration.rs`). This card starts
 * a run, counts down while the room must stay empty, and then shows each
 * node's presence threshold and 0–100 link score.
 *
 * Split in two so the logic can be tested under `node --test` without a DOM:
 * - `MotionCalibrationController` talks to the server and keeps the state.
 * - `MotionCalibrationCard` renders that state. Server values only ever reach
 *   the page through `textContent`, never `innerHTML`.
 *
 * Thresholds and link scores come from a heuristic tuned on SYNTHETIC rooms;
 * the card says so, as the server's own `note` does.
 */

import { API_TOKEN_STORAGE_KEY } from '../services/api.service.js';

export const STATUS_PATH = '/api/v1/calibration/motion';
export const START_PATH = '/api/v1/calibration/motion/start';

/** Poll interval while a run is in progress. */
export const POLL_MS = 1000;

function storedToken() {
  try {
    return localStorage.getItem(API_TOKEN_STORAGE_KEY) || null;
  } catch {
    return null;
  }
}

function authHeaders() {
  const token = storedToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * Text for a failed request. `status` is the HTTP status (0 when the server
 * could not be reached); `body` is the parsed JSON body, if any.
 */
export function requestErrorText(status, body) {
  if (status === 0) return 'Could not reach the RuView server.';
  if (status === 401) {
    return 'The server needs an API token. Add it under Settings → API access, then try again.';
  }
  if (status === 403) {
    return 'This sign-in can only view data. Starting a calibration needs the server\'s API token (RUVIEW_API_TOKEN), added under Settings → API access.';
  }
  if (status === 404) return 'This server version has no motion calibration.';
  if (body && typeof body.error === 'string' && body.error) return body.error;
  return `The server answered HTTP ${status}.`;
}

/** Frames collected so far, summed over nodes (`run.frames` is per node). */
function framesSoFar(run) {
  const frames = run && run.frames;
  if (typeof frames === 'number') return frames;
  if (!frames || typeof frames !== 'object') return 0;
  return Object.values(frames).reduce((sum, n) => sum + (Number(n) || 0), 0);
}

/**
 * Text for a run in progress (`run` from the status endpoint).
 * `run.seconds_remaining` counts to the end of the whole run, waiting time
 * included, so the time until recording starts needs the run's duration:
 * `durationSecs` from the start response, when this page started the run.
 */
export function runPhaseText(run, durationSecs = null) {
  if (!run) return '';
  const remaining = Math.max(0, Number(run.seconds_remaining) || 0);
  switch (run.phase) {
    case 'waiting':
      if (typeof durationSecs === 'number' && durationSecs > 0) {
        return `Leave the room now. Recording starts in ${Math.ceil(Math.max(0, remaining - durationSecs))} s.`;
      }
      return `Leave the room now. The run ends in ${Math.ceil(remaining)} s.`;
    case 'collecting':
      return `Keep the room empty: ${Math.ceil(remaining)} s left (${framesSoFar(run)} frames so far).`;
    default:
      return 'Finishing…';
  }
}

/**
 * One row per node, sorted by node id. Combines the node's current state
 * (`nodes`) with the last run's outcome (`last_run`), so a node whose last
 * run failed still shows the threshold it keeps using.
 */
export function nodeRows(status) {
  const nodes = (status && status.nodes) || {};
  const lastRun = (status && status.last_run) || {};
  const ids = new Set([...Object.keys(nodes), ...Object.keys(lastRun)]);
  return [...ids]
    .sort((a, b) => Number(a) - Number(b))
    .map((id) => {
      const node = nodes[id] || {};
      const outcome = lastRun[id];
      const calibration = node.calibration || null;
      const quality = calibration && calibration.link_quality;
      return {
        nodeId: Number(id),
        calibrated: Boolean(node.calibrated),
        threshold: typeof node.presence_threshold === 'number' ? node.presence_threshold : null,
        noisy: Boolean(calibration && calibration.noisy),
        linkScore: quality && typeof quality.score === 'number' ? quality.score : null,
        snrDb: quality && typeof quality.snr_db === 'number' ? quality.snr_db : null,
        frameRateHz: quality && typeof quality.frame_rate_hz === 'number' ? quality.frame_rate_hz : null,
        lossPct: quality && typeof quality.loss_pct === 'number' ? quality.loss_pct : null,
        lastRunError: outcome && outcome.success === false ? String(outcome.error || 'failed') : null,
      };
    });
}

/** One-line summary of a node row, for screen readers and the row's detail line. */
export function nodeDetailText(row) {
  if (row.lastRunError) return `Last run failed: ${row.lastRunError}`;
  if (!row.calibrated) return 'Not calibrated: using the default threshold.';
  const parts = [];
  if (row.snrDb !== null) parts.push(`SNR ${row.snrDb.toFixed(0)} dB`);
  if (row.frameRateHz !== null) parts.push(`${row.frameRateHz.toFixed(0)} frames/s`);
  if (row.lossPct !== null) parts.push(`${row.lossPct.toFixed(1)}% lost`);
  if (row.noisy) parts.push('noisy: only clear motion registers');
  return parts.join(' · ');
}

/**
 * Talks to the server. `fetchImpl` and the timer functions are injectable for
 * tests. `onChange(state)` is called after every change.
 *
 * state = { status, running, busy, error, message }
 */
export class MotionCalibrationController {
  constructor({ fetchImpl, setTimer, clearTimer, onChange } = {}) {
    this._fetch = fetchImpl || ((...args) => globalThis.fetch(...args));
    this._setTimer = setTimer || ((fn, ms) => setTimeout(fn, ms));
    this._clearTimer = clearTimer || ((id) => clearTimeout(id));
    this._onChange = onChange || (() => {});
    this._timer = null;
    this._stopped = false;
    this._durationSecs = null;
    this.state = { status: null, running: false, busy: false, error: null, message: null };
  }

  _set(patch) {
    this.state = { ...this.state, ...patch };
    this._onChange(this.state);
  }

  async _request(method, path) {
    let response;
    try {
      response = await this._fetch(path, { method, headers: { Accept: 'application/json', ...authHeaders() } });
    } catch {
      return { ok: false, status: 0, body: null };
    }
    let body = null;
    try {
      body = await response.json();
    } catch {
      body = null;
    }
    return { ok: response.ok, status: response.status, body };
  }

  /** Fetch the status once; keep polling while a run is in progress. */
  async refresh() {
    this._clearPoll();
    const { ok, status, body } = await this._request('GET', STATUS_PATH);
    if (this._stopped) return;
    if (!ok) {
      this._set({ error: requestErrorText(status, body), running: false });
      return;
    }
    const running = Boolean(body && body.run);
    const wasRunning = this.state.running;
    this._set({
      status: body,
      running,
      error: null,
      message: running
        ? runPhaseText(body.run, this._durationSecs)
        : (wasRunning ? 'Calibration finished.' : this.state.message),
    });
    if (!running) this._durationSecs = null;
    if (running) this._timer = this._setTimer(() => this.refresh(), POLL_MS);
  }

  /** Start a run with the server's default delay and duration. */
  async start() {
    if (this.state.busy || this.state.running) return;
    this._set({ busy: true, error: null, message: null });
    const { ok, status, body } = await this._request('POST', START_PATH);
    if (this._stopped) return;
    if (!ok) {
      // 409: a run is already going (started elsewhere). Show it instead of failing.
      if (status === 409) {
        this._set({ busy: false });
        await this.refresh();
        return;
      }
      this._set({ busy: false, error: requestErrorText(status, body) });
      return;
    }
    this._durationSecs = body && typeof body.duration_secs === 'number' ? body.duration_secs : null;
    this._set({ busy: false, running: true, message: (body && body.instructions) || null });
    await this.refresh();
  }

  _clearPoll() {
    if (this._timer !== null) {
      this._clearTimer(this._timer);
      this._timer = null;
    }
  }

  stop() {
    this._stopped = true;
    this._clearPoll();
  }
}

/** Renders the controller's state into a `.sensing-card` element. */
export class MotionCalibrationCard {
  /** @param {HTMLElement} root  an empty element to fill */
  constructor(root, controllerOptions = {}) {
    this.root = root;
    this.controller = new MotionCalibrationController({
      ...controllerOptions,
      onChange: (state) => this.render(state),
    });
  }

  init() {
    const doc = this.root.ownerDocument;
    this.root.textContent = '';

    const title = doc.createElement('div');
    title.className = 'sensing-card-title';
    title.textContent = 'Empty-room calibration';

    const help = doc.createElement('p');
    help.className = 'sensing-about-text';
    help.textContent = 'Sets each node\'s presence threshold from a minute of empty room. Start it, leave the room, and keep pets and fans out until it finishes.';

    this.button = doc.createElement('button');
    this.button.type = 'button';
    this.button.className = 'btn btn--primary btn--sm btn--full-width calibration-start';
    this.button.textContent = 'Calibrate empty room';
    this.button.addEventListener('click', () => this.controller.start());

    this.statusLine = doc.createElement('div');
    this.statusLine.className = 'calibration-status';
    this.statusLine.setAttribute('role', 'status');
    this.statusLine.setAttribute('aria-live', 'polite');

    this.list = doc.createElement('div');
    this.list.className = 'calibration-nodes';

    const note = doc.createElement('p');
    note.className = 'calibration-note';
    note.textContent = 'Thresholds and link scores are SYNTHETIC-tuned estimates, not yet validated on hardware.';

    this.root.append(title, help, this.button, this.statusLine, this.list, note);
    this.controller.refresh();
  }

  render(state) {
    if (!this.button) return;
    this.button.disabled = state.busy || state.running;
    this.button.textContent = state.running ? 'Calibrating…' : 'Calibrate empty room';

    this.statusLine.textContent = state.error || state.message || '';
    this.statusLine.classList.toggle('calibration-status--error', Boolean(state.error));

    const doc = this.root.ownerDocument;
    this.list.textContent = '';
    for (const row of nodeRows(state.status)) {
      const item = doc.createElement('div');
      item.className = 'calibration-node';

      const head = doc.createElement('div');
      head.className = 'calibration-node-head';
      const name = doc.createElement('span');
      name.textContent = `Node ${row.nodeId}`;
      const score = doc.createElement('span');
      score.className = 'calibration-score';
      score.textContent = row.linkScore === null ? 'Link score —' : `Link score ${row.linkScore}/100`;
      head.append(name, score);

      const bar = doc.createElement('div');
      bar.className = 'sensing-bar';
      const fill = doc.createElement('div');
      fill.className = 'sensing-bar-fill confidence';
      fill.style.width = `${row.linkScore === null ? 0 : Math.min(100, Math.max(0, row.linkScore))}%`;
      bar.appendChild(fill);

      const detail = doc.createElement('div');
      detail.className = 'calibration-node-detail';
      const threshold = row.threshold === null ? '' : `Threshold ${row.threshold.toFixed(3)} · `;
      detail.textContent = threshold + nodeDetailText(row);

      item.append(head, bar, detail);
      this.list.appendChild(item);
    }
  }

  dispose() {
    this.controller.stop();
  }
}

import { test } from 'node:test';
import assert from 'node:assert/strict';

globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
const { SensingTab } = await import('./SensingTab.js');

/**
 * A document whose <script> loads either succeed or fail, so the Three.js CDN
 * can be "unreachable" without a browser.
 */
function fakeDocument({ cdnReachable }) {
  const appended = [];
  return {
    appended,
    createElement: () => ({}),
    head: {
      appendChild(script) {
        appended.push(script.src);
        queueMicrotask(() => {
          if (cdnReachable) {
            globalThis.window.THREE = {};
            script.onload();
          } else {
            script.onerror();
          }
        });
      },
    },
  };
}

function tabWithStubs() {
  const viewport = { innerHTML: '', clientWidth: 400, clientHeight: 300 };
  const container = {
    innerHTML: '',
    querySelector: (sel) => (sel === '#sensingViewport' ? viewport : null),
  };
  const tab = new SensingTab(container);
  const calls = [];
  tab._connectService = () => calls.push('connect');
  tab._setupResize = () => calls.push('resize');
  return { tab, calls, viewport };
}

test('without the Three.js CDN the tab still connects to live data and says why the 3D view is missing', async () => {
  globalThis.window = {};
  globalThis.document = fakeDocument({ cdnReachable: false });
  const warn = console.warn;
  console.warn = () => {};
  try {
    const { tab, calls, viewport } = tabWithStubs();
    await tab.init(); // used to reject here, before connecting
    assert.deepEqual(calls, ['connect', 'resize']);
    assert.equal(tab.splatRenderer, null);
    assert.match(viewport.innerHTML, /3D view unavailable/);
    assert.match(viewport.innerHTML, /Live readings still update/);

    // app.js calls init() again on every visit while splatRenderer is null:
    // it must not connect a second time or rebuild the tab.
    await tab.init();
    assert.deepEqual(calls, ['connect', 'resize']);
    assert.equal(globalThis.document.appended.length, 1);
  } finally {
    console.warn = warn;
    delete globalThis.window;
    delete globalThis.document;
  }
});

test('live data is connected before Three.js finishes loading', async () => {
  globalThis.window = {};
  globalThis.document = fakeDocument({ cdnReachable: true });
  try {
    const { tab, calls } = tabWithStubs();
    let connectedBeforeThree = null;
    tab._connectService = () => {
      connectedBeforeThree = !globalThis.window.THREE;
      calls.push('connect');
    };
    tab._initSplatRenderer = () => calls.push('renderer');
    await tab.init();
    assert.equal(connectedBeforeThree, true);
    assert.deepEqual(calls, ['connect', 'renderer', 'resize']);
    assert.equal(tab._threeLoaded, true);
  } finally {
    delete globalThis.window;
    delete globalThis.document;
  }
});

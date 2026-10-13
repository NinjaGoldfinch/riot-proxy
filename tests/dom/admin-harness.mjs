// Loads src/ui/admin.html in jsdom against the fake admin API, with a fake
// WebSocket the test can push frames into and read sent frames from, and a
// switchable document.hidden. Every admin-*.test.mjs builds its page here.
import { JSDOM } from 'jsdom';

import { html, api, KEY } from './fake-admin.mjs';

export { KEY };

/**
 * opts: { hash, key, base, theme, prefersLight }
 * returns { w, $, $$, text, calls, sockets, sent, push, setHidden, settle, intervals, errors, close }
 */
export async function page(opts = {}) {
  const calls = [];
  const errors = [];
  const sockets = [];
  const intervals = [];
  let hidden = false;
  const origin = 'http://admin.test';
  // jsdom runs no module scripts; the page's has no top-level await, so a
  // classic script runs it unchanged.
  const dom = new JSDOM(html.replace('<script type="module">', '<script>'), {
    url: `${origin}/dashboard${opts.hash ?? ''}`,
    runScripts: 'dangerously',
    pretendToBeVisual: true,
    beforeParse(w) {
      if (opts.key) w.localStorage.setItem('rp.dashboard.key', opts.key);
      if (opts.base) w.localStorage.setItem('rp.admin.base', opts.base);
      if (opts.theme) w.localStorage.setItem('rp.admin.theme', opts.theme);
      w.matchMedia = (q) => ({ matches: Boolean(opts.prefersLight) && q.includes('light'), addEventListener() {} });
      w.fetch = async (url, init = {}) => {
        const auth = init.headers?.Authorization ?? null;
        const [status, body] = api(calls, String(url), { method: init.method ?? 'GET', auth });
        const text = JSON.stringify(body);
        return { ok: status < 300, status, headers: new Map(), text: async () => text, json: async () => JSON.parse(text) };
      };
      w.WebSocket = class {
        constructor(url) {
          this.url = url;
          this.readyState = 0;
          this.sent = [];
          sockets.push(this);
          setTimeout(() => { this.readyState = 1; this.onopen?.(); }, 0);
        }
        send(s) { this.sent.push(JSON.parse(s)); }
        close() { this.readyState = 3; }
        // Test side: a server frame, or the server closing the socket.
        frame(f) { this.onmessage?.({ data: JSON.stringify(f) }); }
        drop(code = 1006, reason = '') { this.readyState = 3; this.onclose?.({ code, reason }); }
      };
      const setInterval = w.setInterval.bind(w);
      w.setInterval = (fn, ms, ...a) => { intervals.push(ms); return setInterval(fn, ms, ...a); };
      Object.defineProperty(w.document, 'hidden', { configurable: true, get: () => hidden });
      Object.defineProperty(w.document, 'visibilityState', { configurable: true, get: () => (hidden ? 'hidden' : 'visible') });
      w.console.error = (...a) => errors.push(a.map(String).join(' '));
      w.addEventListener('error', (e) => errors.push(e.message));
    },
  });
  const w = dom.window;
  const $ = (s) => w.document.querySelector(s);
  const $$ = (s) => [...w.document.querySelectorAll(s)];
  const settle = async () => { for (let i = 0; i < 20; i++) await new Promise((r) => setTimeout(r, 5)); };
  const text = (s) => ($(s)?.textContent ?? '').replace(/\s+/g, ' ').trim();
  await settle();
  return {
    w, $, $$, text, calls, sockets, intervals, errors, settle,
    socket: () => sockets.at(-1),
    sent: () => sockets.flatMap((s) => s.sent),
    push: (event, data) => sockets.at(-1).frame({ op: 'event', event, topic: 'firehose', at: Date.now(), data }),
    setHidden: async (v) => { hidden = v; w.document.dispatchEvent(new w.Event('visibilitychange')); await settle(); },
    close: () => w.close(),
  };
}

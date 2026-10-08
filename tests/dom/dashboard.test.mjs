// The dashboard's crawl activity (DEV-13) driven in jsdom: history rows open a
// detail with each stage's progress, the crawl's running, next and failed jobs and
// the platform's match downloads; the job queue panel lists what is running and
// what runs next. The analytics panel's Recompute now (DEV-16) queues a rebuild. The fake API serves the documented shapes of
// `GET /v1/admin/ladder/crawls`, `/v1/admin/ladder/crawls/{id}` and
// `/v1/admin/jobs/queue`, `/v1/admin/ladder/options` and
// `POST /v1/admin/analytics/recompute` (see the OpenAPI document); the metrics snapshot is a
// 503, which these panels do not depend on.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { JSDOM } from 'jsdom';

import { html, RUNNING, DONE, api } from './fake-dashboard.mjs';

async function page() {
  const calls = [];
  const errors = [];
  const dom = new JSDOM(html, {
    url: 'http://localhost/dashboard#ladder', runScripts: 'dangerously', pretendToBeVisual: true,
    beforeParse(w) {
      w.fetch = async (url, init = {}) => {
        const [status, body] = api(calls, url, { method: init.method ?? 'GET', body: init.body });
        const text = JSON.stringify(body);
        return { ok: status < 300, status, headers: new Map(), text: async () => text, json: async () => JSON.parse(text) };
      };
      w.WebSocket = class { constructor() { setTimeout(() => this.onclose?.(), 0); } send() {} close() {} };
      w.confirm = () => true;
      w.CSS = { escape: (s) => String(s) };
      w.console.error = (...a) => errors.push(a.map(String).join(' '));
      w.addEventListener('error', (e) => errors.push(e.message));
    },
  });
  const w = dom.window;
  const $ = (s) => w.document.querySelector(s);
  const settle = async () => { for (let i = 0; i < 30; i++) await new Promise((r) => setTimeout(r, 5)); };
  await settle();
  const text = (s) => ($(s)?.textContent ?? '').replace(/\s+/g, ' ');
  return { w, $, calls, errors, settle, text, close: () => w.close() };
}

// The snapshot is a 503 here; anything else logged is a real failure.
const realErrors = (errors) => errors.filter((e) => !e.includes('metrics fetch failed: 503'));

test('history rows are buttons that open and close a detail row', async () => {
  const p = await page();
  try {
    const rows = [...p.w.document.querySelectorAll('#ladderHistory tr[data-run]')];
    assert.equal(rows.length, 2);
    assert.equal(rows[0].getAttribute('tabindex'), '0');
    assert.equal(rows[0].getAttribute('aria-expanded'), 'false');
    p.$(`tr[data-run="${DONE}"] td`).click();
    await p.settle();
    assert.ok(p.calls.includes(`/v1/admin/ladder/crawls/${DONE}`));
    const detail = p.$(`#ladderHistory [data-activity="${DONE}"]`);
    assert.ok(detail, 'a detail row under the run');
    assert.equal(p.$(`tr[data-run="${DONE}"]`).getAttribute('aria-expanded'), 'true');
    assert.match(detail.textContent, /archive\s*done/);
    assert.ok(detail.textContent.includes('21,137 / 21,137 ids handed on'));
    assert.ok(!detail.querySelector('[data-cancel]'), 'a finished crawl has nothing to cancel');
    p.$(`tr[data-run="${DONE}"] td`).click();
    await p.settle();
    assert.equal(p.$(`#ladderHistory [data-activity="${DONE}"]`), null, 'closed again');
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

test('a running crawl shows each stage, what it is running and what runs next', async () => {
  const p = await page();
  try {
    p.$(`tr[data-run="${RUNNING}"] td`).click();
    await p.settle();
    const d = p.$(`#ladderHistory [data-activity="${RUNNING}"]`);
    const t = d.textContent.replace(/\s+/g, ' ');
    assert.match(t, /enumerate\s*done/);
    assert.ok(t.includes('collect') && t.includes('50 / 80 batches of 25 players · 10 in 10m · ~30m 0s left'), t);
    assert.ok(t.includes('archive') && t.includes('after the stage before'));
    const bar = d.querySelector('[role="progressbar"][aria-label="collect"]');
    assert.equal(bar.getAttribute('aria-valuenow'), '63');
    assert.equal(bar.querySelector('i').style.width, '62.5%');
    assert.ok(t.includes('In flight (2)') && t.includes('players 1250+'));
    assert.ok(t.includes('Running now (1)') && t.includes('match ids for players 1,250–1,274 · oc1'));
    assert.ok(t.includes('4 queued job(s) ahead of its next one'));
    assert.match(t, /match ids for players 1,300–1,324 · oc1\s*due in 1m 5\ds · try 2/, 'a backoff shows when it is due');
    assert.ok(t.includes('Failed (1)') && t.includes('RIOT_UNAVAILABLE: match-v5 503'));
    assert.ok(t.includes('Match downloads · every crawl on oc1'));
    assert.ok(t.includes('120 ready · 3 waiting · 2 running · 1 failed · 60 fetched in 10m · ~20m 50s left'));
    assert.ok(d.querySelector(`button[data-cancel="${RUNNING}"]`), 'a running crawl can be cancelled from here');
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

test('cancel from the detail asks the API and refreshes', async () => {
  const p = await page();
  try {
    p.$(`tr[data-run="${RUNNING}"] td`).click();
    await p.settle();
    const before = p.calls.length;
    p.$(`#ladderHistory button[data-cancel="${RUNNING}"]`).click();
    await p.settle();
    assert.ok(p.text('#crawlResult').includes(`Cancelled ${RUNNING}`));
    assert.ok(p.calls.slice(before).includes('/v1/admin/ladder/crawls?limit=50'), 'history refetched');
    assert.ok(p.$(`tr[data-run="${RUNNING}"]`).classList.contains('open'), 'clicking cancel does not fold the row');
  } finally { p.close(); }
});

test('the job queue panel lists what is running and what the workers take next', async () => {
  const p = await page();
  try {
    assert.ok(p.calls.includes('/v1/admin/jobs/queue?limit=15'));
    assert.match(p.text('#queueMeta'), /^2 running · 2 ready · 3 waiting out a backoff · next due in 1m \d+s$/);
    const rows = (sel) => [...p.w.document.querySelectorAll(`${sel} li`)].map((li) => [...li.children].map((c) => c.textContent));
    assert.deepEqual(rows('#queueRunning').map((r) => r.slice(0, 2)), [
      ['ladder:collect', 'match ids for players 1,250–1,274 · oc1'],
      ['archive:match', 'match OC1_700001'],
    ]);
    assert.deepEqual(rows('#queueNext'), [['archive:match', 'match OC1_700002', 'ready'], ['ladder:collect', 'match ids for players 1,275–1,299 · oc1', 'ready']]);
    assert.equal(p.$('#queueNext li').title, 'archive:match · priority 100', 'priority in the tooltip');
  } finally { p.close(); }
});

test('an open running row keeps refreshing; a finished one is fetched once', async () => {
  const p = await page();
  try {
    p.$(`tr[data-run="${RUNNING}"] td`).click();
    p.$(`tr[data-run="${DONE}"] td`).click();
    await p.settle();
    const count = (id) => p.calls.filter((c) => c === `/v1/admin/ladder/crawls/${id}`).length;
    const [run0, done0] = [count(RUNNING), count(DONE)];
    p.w.document.dispatchEvent(new p.w.CustomEvent('dashboard:tab', { detail: 'ladder' }));
    await p.settle();
    assert.equal(count(RUNNING), run0 + 1, 'running: refreshed');
    assert.equal(count(DONE), done0, 'finished: not refetched');
  } finally { p.close(); }
});

test('Recompute now offers the ladder options and queues the chosen ladder', async () => {
  const p = await page();
  try {
    assert.equal(p.$('#analyticsStart').disabled, false, 'enabled once the options load');
    assert.deepEqual([...p.$('#analyticsPlatform').options].map((o) => o.value), ['euw1', 'oc1']);
    assert.equal(p.$('#analyticsPlatform').value, 'oc1', 'the configured default is preselected');
    p.$('#analyticsQueue').value = 'RANKED_FLEX_SR';
    p.$('#analyticsStart').click();
    await p.settle();
    assert.deepEqual(p.calls.bodies, [['/v1/admin/analytics/recompute', { platform: 'oc1', queue: 'RANKED_FLEX_SR' }]]);
    assert.ok(p.text('#analyticsResult').includes('Queued oc1 · RANKED_FLEX_SR'));
    assert.ok(p.$('#analyticsResult').classList.contains('good'));
    assert.equal(p.$('#analyticsStart').disabled, false, 'ready for another');
    p.$('#analyticsPlatform').value = 'euw1';
    p.$('#analyticsPlatform').dispatchEvent(new p.w.Event('change'));
    assert.equal(p.text('#analyticsResult'), '', 'a new choice clears the old answer');
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

test('a refused recompute shows the API message', async () => {
  const p = await page();
  try {
    p.$('#analyticsPlatform').value = 'euw1';
    p.$('#analyticsStart').click();
    await p.settle();
    assert.equal(p.text('#analyticsResult'), 'not in this test: euw1');
    assert.ok(p.$('#analyticsResult').classList.contains('bad'));
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

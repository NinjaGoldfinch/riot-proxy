// The dashboard's Ladder tab driven in jsdom. A running crawl is a card: its stage
// bars and totals, with its legs, jobs, counters and the platform's match downloads
// folded under "details" (DEV-13, DEV-17). Past crawls page ten at a time and each
// row opens into the same view; the job queue panel, folded, lists what is running
// and what runs next. The analytics panel's Recompute now (DEV-16) queues a rebuild. The fake API serves the documented shapes of
// `GET /v1/admin/ladder/crawls`, `/v1/admin/ladder/crawls/{id}` and
// `/v1/admin/jobs/queue`, `/v1/admin/ladder/options` and
// `POST /v1/admin/analytics/recompute` (see the OpenAPI document); the metrics snapshot is a
// 503, which these panels do not depend on.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { JSDOM } from 'jsdom';

import { html, RUNNING, DONE, api, scene } from './fake-dashboard.mjs';

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

test('a running crawl is a card: stage bars and totals, the rest folded under details', async () => {
  const p = await page();
  try {
    const card = p.$(`#ladderRunning [data-activity="${RUNNING}"]`);
    assert.ok(card, 'the running crawl has a card');
    assert.equal(p.$(`#ladderHistory tr[data-run="${RUNNING}"]`), null, 'and is not repeated in the history');
    assert.ok(p.text('#ladderRunning .crawl-head').includes('oc1 · RANKED_SOLO_5x5'));
    assert.ok(p.text('#ladderMeta').startsWith('1 running'));
    const t = card.textContent.replace(/\s+/g, ' ');
    assert.match(t, /enumerate\s*done/);
    assert.ok(t.includes('collect') && t.includes('50 / 80 batches of 25 players · 10 in 10m · ~30m 0s left'), t);
    assert.ok(t.includes('archive') && t.includes('after the stage before'));
    const bar = card.querySelector('[role="progressbar"][aria-label="collect"]');
    assert.equal(bar.getAttribute('aria-valuenow'), '63');
    assert.equal(bar.querySelector('i').style.width, '62.5%');
    assert.equal(p.text(`#ladderRunning .totals`), '1,982 players21,137 match ids0 matches queued1 failed');

    const more = card.querySelector('details.more');
    assert.equal(more.open, false, 'details are folded by default');
    assert.equal(more.querySelector('summary').textContent.replace(/\s+/g, ' '), 'details · 1 running · 2 legs in flight · 1 failed');
    const d = more.querySelector('.body').textContent.replace(/\s+/g, ' ');
    assert.ok(d.includes('histories queued1,982') && d.includes('legs left30'), d);
    assert.ok(d.includes('In flight (2)') && d.includes('players 1250+'));
    assert.ok(d.includes('Running now (1)') && d.includes('match ids for players 1,250–1,274 · oc1'));
    assert.ok(d.includes('4 queued job(s) ahead of its next one'));
    assert.match(d, /match ids for players 1,300–1,324 · oc1\s*due in 1m 5\ds · try 2/, 'a backoff shows when it is due');
    assert.ok(d.includes('Failed (1)') && d.includes('RIOT_UNAVAILABLE: match-v5 503'));
    assert.ok(d.includes('Match downloads · every crawl on oc1'));
    assert.ok(d.includes('120 ready · 3 waiting · 2 running · 1 failed · 60 fetched in 10m · ~20m 50s left'));
    assert.equal(p.$('#ladderRunning').querySelectorAll('[data-cancel]').length, 1, 'one cancel button, in the card head');
    assert.equal(p.$('#crawlForm').open, false, 'a crawl is running: the start form stays folded');
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

test('with nothing running, the card list says so and the start form is open', async () => {
  scene.running = false;
  const p = await page();
  try {
    assert.equal(p.text('#ladderRunning'), 'No crawl running.');
    assert.ok(p.text('#ladderMeta').startsWith('none running'));
    assert.equal(p.$('#crawlForm').open, true);
    assert.equal(p.$('#crawlStart').disabled, false);
  } finally { p.close(); scene.running = true; }
});

test('an opened details fold stays open when the card refreshes', async () => {
  const p = await page();
  try {
    const more = () => p.$(`#ladderRunning details.more`);
    more().open = true;
    more().dispatchEvent(new p.w.Event('toggle'));
    const count = () => p.calls.filter((c) => c === `/v1/admin/ladder/crawls/${RUNNING}`).length;
    const before = count();
    p.w.document.dispatchEvent(new p.w.CustomEvent('dashboard:tab', { detail: 'ladder' }));
    await p.settle();
    assert.equal(count(), before + 1, 'refreshed');
    assert.equal(more().open, true, 'still open');
  } finally { p.close(); }
});

test('past crawls list finished runs ten at a time; a row opens into its detail', async () => {
  const p = await page();
  try {
    const rows = () => [...p.w.document.querySelectorAll('#ladderHistory tr[data-run]')];
    assert.equal(rows().length, 10);
    assert.equal(rows()[0].dataset.run, DONE, 'newest first');
    assert.equal(p.text('#ladderHistoryMeta'), '12 run(s) · 1 failed or cancelled');
    assert.equal(p.text('#historyMore'), 'show 2 more');
    p.$('#historyMore').click();
    assert.equal(rows().length, 12);
    assert.equal(p.text('#historyMore'), 'show fewer');

    assert.equal(rows()[0].getAttribute('tabindex'), '0');
    assert.equal(rows()[0].getAttribute('aria-expanded'), 'false');
    p.$(`tr[data-run="${DONE}"] td`).click();
    await p.settle();
    assert.ok(p.calls.includes(`/v1/admin/ladder/crawls/${DONE}`));
    const detail = p.$(`#ladderHistory [data-activity="${DONE}"]`);
    assert.ok(detail, 'a detail row under the run');
    assert.equal(p.$(`tr[data-run="${DONE}"]`).getAttribute('aria-expanded'), 'true');
    assert.match(detail.textContent, /archive\s*done/);
    assert.ok(detail.textContent.includes('21,137 / 21,137 ids handed on'));
    assert.equal(detail.querySelector('details.more').open, true, 'an opened row shows its details');
    assert.ok(!detail.querySelector('[data-cancel]'), 'a finished crawl has nothing to cancel');
    detail.querySelector('details.more summary').click();
    await p.settle();
    assert.ok(p.$(`#ladderHistory [data-activity="${DONE}"]`), 'folding the details leaves the row open');
    p.$(`tr[data-run="${DONE}"] td`).click();
    await p.settle();
    assert.equal(p.$(`#ladderHistory [data-activity="${DONE}"]`), null, 'closed again');
    assert.deepEqual(realErrors(p.errors), []);
  } finally { p.close(); }
});

test('cancel from the card asks the API and refreshes', async () => {
  const p = await page();
  try {
    const before = p.calls.length;
    p.$(`#ladderRunning button[data-cancel="${RUNNING}"]`).click();
    await p.settle();
    assert.ok(p.text('#crawlResult').includes(`Cancelled ${RUNNING}`));
    assert.ok(p.calls.slice(before).includes('/v1/admin/ladder/crawls?limit=50'), 'history refetched');
  } finally { p.close(); }
});

test('the job queue panel is folded and lists what is running and what the workers take next', async () => {
  const p = await page();
  try {
    assert.equal(p.$('#queueFold').open, false);
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

test('a running crawl keeps refreshing; an opened finished row is fetched once', async () => {
  const p = await page();
  try {
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
    assert.equal(p.$('#analyticsFold').open, false, 'folded by default');
    assert.equal(p.$('#analyticsStart').disabled, false, 'enabled once the options load');
    assert.deepEqual([...p.$('#analyticsPlatform').options].map((o) => o.value), ['euw1', 'oc1']);
    assert.equal(p.$('#analyticsPlatform').value, 'oc1', 'the configured default is preselected');
    p.$('#analyticsQueue').value = 'RANKED_FLEX_SR';
    p.$('#analyticsStart').click();
    await p.settle();
    assert.deepEqual(p.calls.bodies, [['/v1/admin/analytics/recompute', { platform: 'oc1', queue: 'RANKED_FLEX_SR' }]]);
    assert.ok(p.text('#analyticsResult').includes('Queued oc1 · RANKED_FLEX_SR'));
    assert.ok(p.text('#analyticsResult').includes('ahead of the queue'));
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

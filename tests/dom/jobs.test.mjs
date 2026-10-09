// /dev/jobs (DEV-19) driven in jsdom against a fake admin API (fake-jobs.mjs):
// the overview shows each worker's job and latest step, the queue, the counts,
// what finished and what failed; any job opens in its own closable tab that
// follows it with `?after=`, and offers Retry or Cancel where they apply.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { JSDOM } from 'jsdom';

import { html, RUN, DONE, FAILED, PENDING, fake } from './fake-jobs.mjs';

async function page({ hash = '', stored = null } = {}) {
  const { api, calls } = fake();
  const errors = [];
  const dom = new JSDOM(html, {
    url: `http://localhost/dev/jobs${hash}`, runScripts: 'dangerously', pretendToBeVisual: true,
    beforeParse(w) {
      if (stored) w.localStorage.setItem('rp.jobs.tabs', JSON.stringify(stored));
      w.fetch = async (url, init = {}) => {
        const [status, body] = api(url, { method: init.method ?? 'GET', body: init.body });
        const text = JSON.stringify(body);
        return { ok: status < 300, status, headers: new Map(), text: async () => text, json: async () => JSON.parse(text) };
      };
      w.CSS = { escape: (s) => String(s) };
      w.confirm = (q) => { (w.asked ??= []).push(q); return true; };
      w.console.error = (...a) => errors.push(a.map(String).join(' '));
      w.addEventListener('error', (e) => errors.push(e.message));
    },
  });
  const w = dom.window;
  const $ = (s) => w.document.querySelector(s);
  const $$ = (s) => [...w.document.querySelectorAll(s)];
  const settle = async () => { for (let i = 0; i < 30; i++) await new Promise((r) => setTimeout(r, 5)); };
  await settle();
  const text = (s) => ($(s)?.textContent ?? '').replace(/\s+/g, ' ').trim();
  const tabs = () => $$('#tabs [role=tab]').map((t) => t.dataset.tab);
  return { w, $, $$, calls, errors, settle, text, tabs, close: () => w.close() };
}

test('the overview shows each worker, the queue, the counts, what finished and what failed', async () => {
  const p = await page();
  try {
    assert.deepEqual(p.tabs(), ['overview']);
    assert.equal(p.text('#workerSummary'), '1 of 2 busy');
    const busy = p.$(`#workers .worker.busy[data-job="${RUN}"]`);
    assert.ok(busy, 'the busy worker names its job');
    assert.match(busy.textContent, /W1\s*ladder:walk/);
    assert.ok(busy.textContent.includes('GOLD II page 1 on oc1'), 'its latest step');
    assert.match(p.$$('#workers .worker')[1].textContent, /W2\s*idle/);

    assert.ok(p.text('#queueSummary').includes('1 waiting out a backoff'));
    const next = p.$(`#queue tr[data-job="${PENDING}"]`);
    assert.ok(next.textContent.includes('backfill:player'));
    assert.ok(next.textContent.includes('oc1 · puuid pppppppp… · limit 100'), next.textContent);

    // Only kinds with something pending, running or failed.
    const kinds = p.$$('#kinds tbody tr').map((tr) => [...tr.cells].map((c) => c.textContent));
    assert.deepEqual(kinds, [['ladder:walk', '1', '0', '0'], ['archive:match', '0', '1,200', '1']]);
    assert.ok(p.$(`#finished tr[data-job="${DONE}"]`).textContent.includes('done'));
    assert.ok(p.$(`#failed tr[data-job="${FAILED}"]`).textContent.includes('RIOT_UNAVAILABLE: match-v5 503'));
    assert.deepEqual(p.errors, []);
  } finally {
    p.close();
  }
});

test('a worker opens its job in a tab that follows it with after=', async () => {
  const p = await page();
  try {
    p.$(`#workers [data-job="${RUN}"]`).click();
    await p.settle();
    assert.deepEqual(p.tabs(), ['overview', RUN]);
    assert.equal(p.$(`#tabs [data-tab="${RUN}"]`).getAttribute('aria-selected'), 'true');
    assert.ok(p.$('#overview').hidden && !p.$('#jobView').hidden);
    assert.equal(p.w.location.hash, `#job/${RUN}`);
    assert.ok(p.calls.some((c) => c.path === `/v1/admin/jobs/${RUN}/activity?after=0`));
    assert.ok(p.text('#jobView [data-f="now"]').startsWith('GOLD II page 1 on oc1'));
    assert.equal(p.text('#jobView [data-f="pill"]'), 'running');
    assert.ok(p.text('#jobView [data-f="payload"]').includes('"tier": "GOLD"'));
    const lines = () => p.$$('#jobView ol.timeline li').map((li) => li.querySelector('.x').textContent);
    assert.equal(lines().length, 3);
    assert.ok(p.$('#jobView ol.timeline li.ev-riot .ms').textContent.includes('410 ms'));

    // Selecting the tab again polls at once: only the new event is asked for and added.
    p.$(`#tabs [data-tab="${RUN}"]`).click();
    await p.settle();
    assert.ok(p.calls.some((c) => c.path === `/v1/admin/jobs/${RUN}/activity?after=3`));
    assert.equal(lines().at(-1), 'GOLD II page 2 on oc1');
    assert.equal(lines().filter((l) => l === 'GOLD II page 1 on oc1').length, 1, 'nothing twice');
    assert.equal(lines().length, 4);
    assert.ok(p.text('#jobView [data-f="now"]').startsWith('GOLD II page 2 on oc1'));

    // A second job opens beside it; × closes one and the other stays.
    p.$(`#tabs [data-tab="overview"]`).click();
    await p.settle();
    p.$(`#finished tr[data-job="${DONE}"] td`).click();
    await p.settle();
    assert.deepEqual(p.tabs(), ['overview', RUN, DONE]);
    assert.ok(p.text('#jobView [data-f="now"]').startsWith('Ended'));
    assert.ok(p.text('#jobView [data-f="runinfo"]').includes('took 1.0 s'));
    assert.ok(p.text('#jobView ol.timeline').includes('waited for the americas rate limit'));
    p.$(`#tabs [data-close="${DONE}"]`).click();
    await p.settle();
    assert.deepEqual(p.tabs(), ['overview', RUN]);
    assert.ok(!p.$('#overview').hidden, 'closing the open tab goes back to the overview');
    assert.deepEqual(JSON.parse(p.w.localStorage.getItem('rp.jobs.tabs')).map((t) => t.id), [RUN]);
    assert.deepEqual(p.errors, []);
  } finally {
    p.close();
  }
});

test('tabs come back after a reload, and a link opens its job', async () => {
  const p = await page({ hash: `#job/${FAILED}`, stored: [{ id: RUN, kind: 'ladder:walk' }] });
  try {
    assert.deepEqual(p.tabs(), ['overview', RUN, FAILED]);
    assert.equal(p.$(`#tabs [data-tab="${FAILED}"]`).getAttribute('aria-selected'), 'true');
    // Failed and never run here: the row, no trace, and Retry.
    assert.ok(p.text('#jobView [data-f="facts"]').includes('RIOT_UNAVAILABLE: match-v5 503'));
    assert.ok(p.text('#jobView ol.timeline').includes('Nothing recorded'));
    const retry = p.$('#jobView [data-act="retry"]');
    assert.ok(!retry.hidden && p.$('#jobView [data-act="cancel"]').hidden);
    retry.click();
    await p.settle();
    assert.match(p.w.asked[0], /^Run archive:match .* again now\?$/);
    assert.ok(p.calls.some((c) => c.method === 'POST' && c.path === `/v1/admin/jobs/${FAILED}/retry`));
    assert.equal(p.text('#jobView [data-f="msg"]'), 'queued again');
    assert.equal(p.text('#jobView [data-f="pill"]'), 'pending');
    assert.ok(p.text('#jobView [data-f="now"]').startsWith('Waiting for a worker'));
    // Now pending, it can be cancelled instead.
    const cancel = p.$('#jobView [data-act="cancel"]');
    assert.ok(!cancel.hidden && p.$('#jobView [data-act="retry"]').hidden);
    cancel.click();
    await p.settle();
    assert.ok(p.calls.some((c) => c.method === 'DELETE' && c.path === `/v1/admin/jobs/${FAILED}`));
    assert.equal(p.text('#jobView [data-f="msg"]'), 'cancelled');
    assert.deepEqual(p.errors, []);
  } finally {
    p.close();
  }
});

test('a job that does not exist says so', async () => {
  const p = await page({ hash: '#job/01K00000000000000000000000' });
  try {
    assert.equal(p.text('#jobView [data-f="now"]'), 'No such job.');
    assert.deepEqual(p.errors, []);
  } finally {
    p.close();
  }
});

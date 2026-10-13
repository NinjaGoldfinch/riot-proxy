// The Traffic tab (ADM-09) in jsdom: requests by route from the snapshot with
// p50/p95 from the recent ring, Riot calls by region, method and status, the
// live recent-requests table and its filters, and a 5 s poll that runs only
// while the tab is open and visible.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot } from './fake-admin.mjs';

const recentCalls = (p) => p.calls.filter((c) => c.url.includes('/v1/admin/recent?kind=requests')).length;

test('by route: counts by status class, the cache hit share, p50 and p95', async () => {
  scene.snapshot = snapshot({
    requests: {
      '/v1/lol/summoners/by-puuid/:platform/:puuid|200|hit': 90,
      '/v1/lol/summoners/by-puuid/:platform/:puuid|200|miss': 10,
      '/v1/lol/summoners/by-puuid/:platform/:puuid|404|neg': 5,
      '/v1/admin/stats|200|none': 7,
      '/v1/admin/stats|502|none': 1,
    },
  });
  const p = await page({ key: KEY, hash: '#traffic' });
  try {
    assert.equal(p.$('#tab-traffic').getAttribute('aria-selected'), 'true');
    const rows = p.$$('#trRoutes tbody tr').map((tr) => [...tr.children].map((td) => td.textContent.trim()));
    assert.equal(rows.length, 2);
    // The busiest route first: 105 requests, 95 of 105 cacheable reads served.
    assert.deepEqual(rows[0].slice(0, 6), ['/v1/lol/summoners/by-puuid/:platform/:puuid', '105', '100', '5', '0', '90 %']);
    assert.match(rows[0][6], /^\d+ ms$/);
    assert.deepEqual(rows[1].slice(0, 6), ['/v1/admin/stats', '8', '7', '0', '1', '—']);
    const up = p.$$('#trUpstream tbody tr').map((tr) => [...tr.children].map((td) => td.textContent.trim()).join(' '));
    assert.deepEqual(up, ['sea match.byId 200 400']);
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('recent requests: newest first, filtered by text, status class and cache', async () => {
  const p = await page({ key: KEY, hash: '#traffic' });
  try {
    assert.equal(p.$$('#trRecent tbody tr').length, 60);
    assert.equal(p.text('#trMeta'), '60 of the last 60 · every 5 s');
    const status = p.$('#trStatus');
    status.value = '5xx';
    status.dispatchEvent(new p.w.Event('change'));
    const fives = p.$$('#trRecent tbody tr');
    assert.equal(fives.length, 3);
    assert.ok(fives.every((tr) => tr.children[1].textContent.trim() === '502'), 'status sits beside the time');
    assert.ok(fives[0].querySelector('td svg use'), 'a 5xx carries an icon, not colour alone');
    status.value = 'all';
    status.dispatchEvent(new p.w.Event('change'));
    const text = p.$('#trText');
    text.value = 'hide on bush';
    text.dispatchEvent(new p.w.Event('input'));
    assert.equal(p.$$('#trRecent tbody tr').length, 20);
    const cache = p.$('#trCache');
    cache.value = 'none';
    cache.dispatchEvent(new p.w.Event('change'));
    assert.equal(p.text('#trRecent tbody'), 'Nothing matches the filter.');
  } finally { p.close(); }
});

test('the ring is polled every 5 s while the tab is open and visible, and never otherwise', async (t) => {
  const p = await page({ key: KEY });
  try {
    assert.equal(recentCalls(p), 0, 'not on the overview');
    p.w.location.hash = '#traffic';
    await p.settle();
    assert.equal(recentCalls(p), 1, 'once on entry');
    // jsdom's timers are real; check the scheduled poll rather than wait 5 s.
    assert.ok(p.w.rpAdmin.state.tab === 'traffic');
    await p.setHidden(true);
    const hidden = recentCalls(p);
    await new Promise((r) => setTimeout(r, 5200));
    assert.equal(recentCalls(p), hidden, 'nothing while hidden');
    await p.setHidden(false);
    assert.equal(recentCalls(p), hidden + 1, 'refetched on return');
    await new Promise((r) => setTimeout(r, 5200));
    await p.settle();
    assert.equal(recentCalls(p), hidden + 2, 'then every 5 s');
    p.w.location.hash = '#overview';
    await p.settle();
    const left = recentCalls(p);
    await new Promise((r) => setTimeout(r, 5200));
    assert.equal(recentCalls(p), left, 'leaving the tab stops it');
  } finally { p.close(); }
});

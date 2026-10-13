// The Events tab (ADM-15) in jsdom: the firehose kept in a 2,000-frame ring,
// per-name filters with metrics.snapshot off by default, search, pause with a
// count of what arrived meanwhile and no list writes until resumed, a JSON
// view per frame, and an unseen badge while the tab is elsewhere.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { snapshot } from './fake-admin.mjs';

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const flush = () => wait(300);

test('10,000 frames keep the newest 2,000, newest first', async () => {
  const p = await page({ key: KEY, hash: '#events' });
  try {
    for (let i = 0; i < 10000; i++) p.push('match.archived', { matchId: `OC1_${i}` });
    await flush();
    const rows = p.$$('#evList li');
    assert.equal(rows.length, 2000);
    assert.match(rows[0].textContent, /OC1_9999/);
    assert.match(rows.at(-1).textContent, /OC1_8000/);
    assert.equal(p.text('#evMeta'), '2,000 shown of the last 2,000 · ring of 2,000');
  } finally { p.close(); }
});

test('metrics.snapshot frames are hidden until switched on; names filter and search narrow the list', async () => {
  const p = await page({ key: KEY, hash: '#events' });
  try {
    p.push('metrics.snapshot', snapshot());
    p.push('crawl.phase', { crawlId: 'C1', phase: 'collect' });
    p.push('rank.changed', { puuid: 'abc', tier: 'GOLD' });
    await flush();
    assert.deepEqual(p.$$('#evList li code').map((c) => c.textContent), ['rank.changed', 'crawl.phase']);
    assert.equal(p.$('#evNames input[value="metrics.snapshot"]').checked, false);
    const snap = p.$('#evNames input[value="metrics.snapshot"]');
    snap.checked = true;
    snap.dispatchEvent(new p.w.Event('change', { bubbles: true }));
    assert.ok(p.$$('#evList li code').some((c) => c.textContent === 'metrics.snapshot'));
    assert.equal(p.text('[data-count="crawl.phase"]'), '1', 'each name says how many it has');
    p.$('#evSearch').value = 'GOLD';
    p.$('#evSearch').dispatchEvent(new p.w.Event('input'));
    assert.deepEqual(p.$$('#evList li code').map((c) => c.textContent), ['rank.changed']);
    p.$('#evSearch').value = 'nothing like this';
    p.$('#evSearch').dispatchEvent(new p.w.Event('input'));
    assert.equal(p.text('#evList'), 'Nothing matches.');
  } finally { p.close(); }
});

test('paused: nothing is written to the list, the count of what arrived shows, resume draws it', async () => {
  const p = await page({ key: KEY, hash: '#events' });
  try {
    p.push('patch.new', { patch: '16.20' });
    await flush();
    p.$('#evPause').click();
    assert.equal(p.$('#evPause').getAttribute('aria-pressed'), 'true');
    let writes = 0;
    const mo = new p.w.MutationObserver((m) => { writes += m.length; });
    mo.observe(p.$('#evList'), { childList: true, subtree: true, characterData: true, attributes: true });
    for (let i = 0; i < 10000; i++) p.push('match.archived', { matchId: `OC1_${i}` });
    await flush();
    await p.settle();
    mo.disconnect();
    assert.equal(writes, 0, 'no DOM writes to the list while paused');
    assert.equal(p.text('#evPause'), 'Resume (10,000 while paused)');
    assert.equal(p.$$('#evList li').length, 1);
    p.$('#evPause').click();
    assert.equal(p.$$('#evList li').length, 2000);
    assert.equal(p.text('#evPause'), 'Pause');
  } finally { p.close(); }
});

test('a frame opens into its JSON, and stays open as newer frames arrive', async () => {
  const p = await page({ key: KEY, hash: '#events' });
  try {
    p.push('game.started', { puuid: 'abc', gameId: 7 });
    await flush();
    const b = p.$('#evList li button');
    b.click();
    const pre = p.$('#evList li pre');
    assert.ok(pre);
    assert.deepEqual(JSON.parse(pre.textContent).data, { puuid: 'abc', gameId: 7 });
    assert.equal(p.$('#evList li button').getAttribute('aria-expanded'), 'true');
    p.push('game.ended', { puuid: 'abc' });
    await flush();
    assert.equal(p.$$('#evList li')[1].querySelector('pre') !== null, true, 'the opened frame is still open, one row down');
  } finally { p.close(); }
});

test('frames arriving while the tab is elsewhere raise the unseen badge; opening it clears', async () => {
  const p = await page({ key: KEY });
  try {
    p.push('metrics.snapshot', snapshot());
    p.push('crawl.phase', { crawlId: 'C1' });
    p.push('match.archived', { matchId: 'X' });
    await flush();
    assert.equal(p.text('#badge-events'), '2', 'snapshots are not counted');
    assert.equal(p.$('#badge-events').getAttribute('aria-label'), '2 new events');
    p.w.location.hash = '#events';
    await p.settle();
    assert.equal(p.$('#badge-events').hidden, true);
    assert.equal(p.$$('#evList li').length, 2, 'what came before is there');
  } finally { p.close(); }
});

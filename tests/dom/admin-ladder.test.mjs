// The Ladder tab (ADM-12) in jsdom: ADR-087/091's crawl views moved from v1's
// dashboard. The eight cases of the old dashboard.test.mjs keep their names as
// the checklist (its ninth, the job queue panel, is the Jobs tab's now); two
// more cover crawl.phase frames and starting a crawl.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot, ladderWorld, ladderSnapshot, CRAWL, PAST } from './fake-admin.mjs';

beforeEach(() => { scene.ladder = ladderWorld(); scene.snapshot = ladderSnapshot(); });
const ladderPage = async (hash = '#ladder') => {
  const p = await page({ key: KEY, hash });
  await p.settle();
  return p;
};
const crawlCalls = (p, id) => p.calls.filter((c) => c.url.endsWith(`/v1/admin/ladder/crawls/${id}`) && c.method === 'GET').length;

test('a running crawl is a card: stage bars and totals, the rest folded under details', async () => {
  const p = await ladderPage();
  try {
    const card = p.$(`#ladderRunning [data-activity="${CRAWL}"]`);
    assert.ok(card, 'the running crawl has a card');
    assert.equal(p.$(`#ladderHistory tr[data-run="${CRAWL}"]`), null, 'and is not repeated in the history');
    assert.match(p.words('#ladderRunning .crawl-head'), /^oc1 · RANKED_SOLO_5x5 down to MASTER collect started 60m ago Cancel$/);
    assert.ok(p.text('#ladderMeta').startsWith('1 running · 48,210 league entries stored'));
    assert.equal(p.text('#badge-ladder'), '1');
    const t = p.words(`#ladderRunning [data-activity="${CRAWL}"] .stages`);
    assert.equal(t, 'enumerate done 3 / 3 legs collect now 50 / 80 batches of 25 players · 10 in 10m · ~30m 0s left archive waiting after the stage before');
    const bar = card.querySelector('[role="progressbar"][aria-label="collect"]');
    assert.equal(bar.getAttribute('aria-valuenow'), '63');
    assert.equal(bar.querySelector('i').style.width, '62.5%');
    assert.equal(p.words(`#ladderRunning .totals`), '1,982 players 21,137 match ids 0 matches queued 1 failed');
    assert.equal(card.querySelector('.capped'), null, 'an uncapped crawl has no cap note');
    const more = card.querySelector('details.more');
    assert.equal(more.open, false, 'details are folded by default');
    assert.equal(more.querySelector('summary').textContent, 'details · 1 running · 2 legs in flight · 1 failed');
    const d = p.words(`#ladderRunning details.more .body`);
    assert.ok(d.includes('histories queued 1,982 players skipped 340') && d.includes('legs left 30'), d);
    assert.ok(d.includes('In flight (2) players 1250+ players 1275+'));
    assert.ok(d.includes('Running now (1) match ids for players 1,250–1,274 · oc1'));
    assert.ok(d.includes('4 queued job(s) ahead of its next one'));
    assert.match(d, /match ids for players 1,300–1,324 · oc1 due in 1m 5\ds try 2/, 'a backoff shows when it is due');
    assert.ok(d.includes('Failed (1)') && d.includes('RIOT_UNAVAILABLE: match-v5 503'));
    assert.ok(d.includes('Match downloads · every crawl on oc1 120 ready · 3 waiting · 2 running · 1 failed · 60 fetched in 10m · ~20m 50s left'));
    assert.equal(p.$$('#ladderRunning [data-cancel]').length, 1, 'one cancel button, in the card head');
    assert.equal(p.$('#crawlForm').open, false, 'a crawl is running: the start form stays folded');
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('with nothing running, the card list says so and the start form is open', async () => {
  scene.ladder.running = false;
  scene.snapshot = ladderSnapshot(false);
  const p = await ladderPage();
  try {
    assert.equal(p.text('#ladderRunning'), 'No crawl running.');
    assert.ok(p.text('#ladderMeta').startsWith('none running'));
    assert.equal(p.$('#crawlForm').open, true);
    assert.equal(p.$('#crawlStart').disabled, false);
    assert.equal(p.$('#badge-ladder').hidden, true);
  } finally { p.close(); }
});

test('an opened details fold stays open when the card refreshes', async () => {
  const p = await ladderPage();
  try {
    const more = () => p.$('#ladderRunning details.more');
    more().open = true;
    more().dispatchEvent(new p.w.Event('toggle'));
    const before = crawlCalls(p, CRAWL);
    p.push('crawl.phase', { crawlId: CRAWL, phase: 'collect' });
    await p.settle();
    assert.equal(crawlCalls(p, CRAWL), before + 1, 'refreshed');
    assert.equal(more().open, true, 'still open');
  } finally { p.close(); }
});

test('past crawls list finished runs ten at a time; a row opens into its detail', async () => {
  const p = await ladderPage();
  try {
    const rows = () => p.$$('#ladderHistory tr[data-run]');
    assert.equal(rows().length, 10);
    assert.equal(rows()[0].dataset.run, PAST, 'newest first');
    assert.equal(p.text('#ladderHistoryMeta'), '12 run(s) · 1 failed or cancelled');
    assert.equal(p.text('#historyMore'), 'show 2 more');
    p.$('#historyMore').click();
    assert.equal(rows().length, 12);
    assert.equal(p.text('#historyMore'), 'show fewer');
    assert.ok(rows()[4].querySelector('.pill[data-level="critical"] svg'), 'a failed run says so with an icon');

    assert.equal(rows()[0].getAttribute('tabindex'), '0');
    assert.equal(rows()[0].getAttribute('aria-expanded'), 'false');
    p.$(`tr[data-run="${PAST}"] td`).click();
    await p.settle();
    assert.equal(p.w.location.hash, `#ladder/${PAST}`, 'an opened run has its own link');
    assert.ok(crawlCalls(p, PAST) >= 1);
    const detail = p.$(`#ladderHistory [data-activity="${PAST}"]`);
    assert.ok(detail, 'a detail row under the run');
    assert.equal(p.$(`tr[data-run="${PAST}"]`).getAttribute('aria-expanded'), 'true');
    assert.match(p.words(`#ladderHistory [data-activity="${PAST}"] .stages`), /archive done 21,137 \/ 21,137 ids handed on/);
    // Its Master list came back at Riot's cap: the note sits next to the player count.
    assert.equal(p.words(`#ladderHistory [data-activity="${PAST}"] .totals`), '11,000 players Master: top 10,000 only (Riot API limit) 21,137 match ids 0 matches queued');
    assert.equal(detail.querySelectorAll('.totals .capped').length, 1);
    assert.ok(detail.querySelector('.totals .capped svg use'), 'the cap note has an icon');
    assert.equal(detail.querySelector('details.more').open, true, 'an opened row shows its details');
    assert.ok(!detail.querySelector('[data-cancel]'), 'a finished crawl has nothing to cancel');
    detail.querySelector('details.more summary').click();
    await p.settle();
    assert.ok(p.$(`#ladderHistory [data-activity="${PAST}"]`), 'folding the details leaves the row open');
    p.$(`tr[data-run="${PAST}"] td`).click();
    await p.settle();
    assert.equal(p.$(`#ladderHistory [data-activity="${PAST}"]`), null, 'closed again');
    assert.equal(p.w.location.hash, '#ladder');
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('cancel from the card asks the API and refreshes', async () => {
  const p = await ladderPage();
  try {
    p.$(`#ladderRunning button[data-cancel="${CRAWL}"]`).click();
    await p.settle();
    assert.match(p.text('#confirmText'), new RegExp(`^Cancel crawl ${CRAWL}\\?`));
    p.$('#confirmOk').click();
    await p.settle();
    assert.ok(p.calls.some((c) => c.method === 'DELETE' && c.url.endsWith(`/v1/admin/ladder/crawls/${CRAWL}`)));
    assert.equal(p.text('#crawlResult'), `Cancelled ${CRAWL}`);
    const after = p.calls.findLastIndex((c) => c.method === 'DELETE');
    assert.ok(p.calls.slice(after).some((c) => c.url.endsWith('/v1/admin/ladder/crawls?limit=50')), 'history refetched');
  } finally { p.close(); }
});

test('a running crawl keeps refreshing; an opened finished row is fetched once', async () => {
  const p = await ladderPage(`#ladder/${PAST}`);
  try {
    const [run0, done0] = [crawlCalls(p, CRAWL), crawlCalls(p, PAST)];
    assert.equal(done0, 1);
    p.push('crawl.phase', { crawlId: CRAWL, phase: 'archive' });
    await p.settle();
    assert.equal(crawlCalls(p, CRAWL), run0 + 1, 'running: refreshed');
    assert.equal(crawlCalls(p, PAST), done0, 'finished: not refetched');
  } finally { p.close(); }
});

test('Recompute now offers the ladder options and queues the chosen ladder', async () => {
  const p = await ladderPage();
  try {
    assert.equal(p.$('#analyticsFold').open, false, 'folded by default');
    assert.equal(p.$('#analyticsStart').disabled, false, 'enabled once the options load');
    assert.deepEqual([...p.$('#analyticsPlatform').options].map((o) => o.value), ['euw1', 'oc1']);
    assert.equal(p.$('#analyticsPlatform').value, 'oc1', 'the configured default is preselected');
    assert.match(p.words('#analyticsRuns'), /oc1 RANKED_SOLO_5x5 10m ago champions 0\.5 · matchups 1\.3 10 30/);
    assert.equal(p.text('#analyticsTop'), 'Most played, newest patch: Ahri 6g 50%');
    p.$('#analyticsQueue').value = 'RANKED_FLEX_SR';
    p.$('#analyticsForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    p.$('#confirmOk').click();
    await p.settle();
    assert.deepEqual(scene.ladder.bodies, [['/v1/admin/analytics/recompute', { platform: 'oc1', queue: 'RANKED_FLEX_SR' }]]);
    assert.ok(p.text('#analyticsResult').includes('Queued oc1 · RANKED_FLEX_SR'));
    assert.ok(p.text('#analyticsResult').includes('ahead of the queue'));
    assert.equal(p.$('#analyticsResult').dataset.level, 'good');
    assert.equal(p.$('#analyticsStart').disabled, false, 'ready for another');
    p.$('#analyticsPlatform').value = 'euw1';
    p.$('#analyticsPlatform').dispatchEvent(new p.w.Event('change'));
    assert.equal(p.text('#analyticsResult'), '', 'a new choice clears the old answer');
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('a refused recompute shows the API message', async () => {
  const p = await ladderPage();
  try {
    p.$('#analyticsPlatform').value = 'euw1';
    p.$('#analyticsForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    p.$('#confirmOk').click();
    await p.settle();
    assert.equal(p.text('#analyticsResult'), 'not in this test: euw1');
    assert.equal(p.$('#analyticsResult').dataset.level, 'critical');
    assert.ok(p.$('#analyticsResult svg use'), 'with an icon');
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('starting a crawl asks first, posts the choice and says what started', async () => {
  const p = await ladderPage();
  try {
    assert.deepEqual([...p.$('#crawlTierFloor').options].map((o) => o.value).slice(0, 3), ['CHALLENGER', 'GRANDMASTER', 'MASTER']);
    assert.equal(p.$('#crawlTierFloor').value, 'MASTER');
    assert.match(p.text('#crawlHint'), /100 matches per player/);
    p.$('#crawlPlatform').value = 'euw1';
    p.$('#crawlStartForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    assert.equal(p.text('#confirmText'), 'Crawl euw1 · RANKED_SOLO_5x5 down to MASTER? It uses the bulk share of the rate limit until it finishes.');
    p.$('#confirmCancel').click();
    await p.settle();
    assert.deepEqual(scene.ladder.bodies, [], 'declined: nothing posted');
    p.$('#crawlStartForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    p.$('#confirmOk').click();
    await p.settle();
    assert.deepEqual(scene.ladder.bodies, [['/v1/admin/ladder/crawl', { platform: 'euw1', queue: 'RANKED_SOLO_5x5', tierFloor: 'MASTER' }]]);
    assert.equal(p.text('#crawlResult'), 'Started euw1 · RANKED_SOLO_5x5: 01K0000000000000000000NEW1 · 7 legs');
  } finally { p.close(); }
});

test('a crawl.phase frame elsewhere does nothing; leaving the tab stops the refresh', async () => {
  const p = await ladderPage();
  try {
    p.w.location.hash = '#overview';
    await p.settle();
    const before = crawlCalls(p, CRAWL);
    p.push('crawl.phase', { crawlId: CRAWL, phase: 'archive' });
    await new Promise((r) => setTimeout(r, 5200));
    assert.equal(crawlCalls(p, CRAWL), before);
  } finally { p.close(); scene.snapshot = snapshot(); }
});

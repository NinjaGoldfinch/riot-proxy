// The Limiter tab (ADM-10) in jsdom: a card per scope, frozen and held scopes
// first, app windows and method buckets as meters whose used / limit is text,
// a frozen countdown that runs on the clock without refetching, folds that
// survive the next snapshot, and /v1/admin/limits/{scope} read on expand.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot, limiterSnapshot } from './fake-admin.mjs';

async function limiterPage() {
  scene.snapshot = limiterSnapshot();
  const p = await page({ key: KEY, hash: '#limiter' });
  return p;
}

test('a card per scope: frozen first, then held, then the fullest; the summary counts them', async () => {
  const p = await limiterPage();
  try {
    assert.deepEqual(p.$$('.scope').map((c) => c.dataset.scope), ['kr', 'europe', 'oc1']);
    assert.equal(p.words('#limSummary'), '3 scopes 1 frozen 2 holding bulk work 3 interactive waiting bulk ceiling 80 % , 95 % after 30 s idle client wait budget 2,000 ms');
    const kr = p.$('.scope[data-scope="kr"]');
    assert.equal(p.words('.scope[data-scope="kr"] header'), 'Korea kr platform frozen 30s 1 waiting');
    assert.equal(kr.querySelector('.pill').dataset.level, 'serious');
    assert.equal(p.words('.scope[data-scope="europe"] header'), 'Europe europe region bulk held 12s 2 waiting');
    assert.match(p.$('.scope[data-scope="oc1"] header').textContent, /open/);
    assert.equal(p.text('#badge-limiter'), '1', 'the rail counts frozen scopes');
    assert.equal(p.$('#badge-limiter').getAttribute('aria-label'), '1 frozen');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('meters carry used / limit as text, a role and values, and an icon when nearly full', async () => {
  const p = await limiterPage();
  try {
    const krMeters = p.$$('.scope[data-scope="kr"] > .meters .meter');
    assert.deepEqual(krMeters.map((m) => m.textContent.replace(/\s+/g, ' ').trim()), ['app · 1 s 20 / 20', 'app · 2 min 99 / 100']);
    const m = krMeters[0];
    assert.equal(m.getAttribute('role'), 'meter');
    assert.deepEqual(['aria-valuenow', 'aria-valuemax', 'aria-valuetext'].map((a) => m.getAttribute(a)), ['20', '20', '20 of 20']);
    assert.equal(m.querySelector('i').dataset.level, 'critical');
    assert.ok(m.querySelector('.mt svg use'), 'not colour alone');
    const oc = p.$('.scope[data-scope="oc1"] > .meters .meter');
    assert.equal(oc.querySelector('i').dataset.level, 'ok');
    assert.equal(oc.querySelector('.mt svg'), null);
    assert.equal(p.$('.scope[data-scope="europe"] > .meters .meter:last-child i').dataset.level, 'warning');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('methods fullest first, four shown, the rest folded; a held method says so', async () => {
  const p = await limiterPage();
  try {
    const oc = p.$('.scope[data-scope="oc1"]');
    const shown = [...oc.querySelectorAll(':scope > .meters .meter .ml')].slice(2).map((e) => e.textContent);
    assert.deepEqual(shown, ['league.entries · 10 s', 'summoner.byPuuid · 10 s', 'match.ids · 10 s', 'spectator · 10 s']);
    const fold = oc.querySelector('details[data-fold="m:oc1"]');
    assert.equal(fold.open, false);
    assert.equal(fold.querySelector('summary').textContent, '2 more methods');
    assert.match(p.text('.scope[data-scope="europe"]'), /bulk held on this method/);
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('the frozen countdown runs on the clock without a refetch', async () => {
  const p = await limiterPage();
  try {
    const cd = () => p.$('.scope[data-scope="kr"] [data-until]').textContent;
    assert.equal(cd(), '30s');
    const calls = p.calls.length;
    await new Promise((r) => setTimeout(r, 2100));
    assert.match(cd(), /^2[78]s$/);
    assert.equal(p.calls.length, calls, 'nothing fetched');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('live usage reads /v1/admin/limits/{scope} on expand and folds stay open across snapshots', async () => {
  const p = await limiterPage();
  try {
    const fold = p.$('details[data-live="kr"]');
    assert.ok(!p.calls.some((c) => c.url.includes('/v1/admin/limits/')), 'nothing before it opens');
    fold.open = true;
    await p.settle();
    assert.ok(p.calls.some((c) => c.url.endsWith('/v1/admin/limits/kr')));
    assert.match(p.words('details[data-live="kr"] .live'), /app · 1 s 4 \/ 20 app · 2 min 40 \/ 100 read \d\d:\d\d:\d\d · frozen 25s/);
    p.$('details[data-fold="m:oc1"]').open = true;
    p.push('metrics.snapshot', limiterSnapshot());
    await p.settle();
    assert.equal(p.$('details[data-live="kr"]').open, true, 'still open after a redraw');
    assert.equal(p.$('details[data-fold="m:oc1"]').open, true);
    assert.match(p.text('details[data-live="kr"] .live'), /4 \/ 20/, 'the last reading is kept');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

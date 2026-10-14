// The Overview's charts and recent errors (ADM-09) in jsdom: 24 h of history
// drawn once per tab entry as one <path> per series, extended by snapshots,
// points from before ADM-04 (no disk) drawn as a gap, a crosshair readout by
// keyboard, a table toggle on every chart, and the errors panel.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot } from './fake-admin.mjs';

const CHARTS = ['chArchive', 'chBacklog', 'chHit', 'chDisk', 'chRequests', 'chUpstream'];

test('1440 history points: six charts, each one <path> per series, history fetched once', async () => {
  const p = await page({ key: KEY });
  try {
    assert.deepEqual(p.$$('#charts .chart').map((c) => c.id), CHARTS);
    for (const id of CHARTS) {
      const lines = p.$$(`#${id} path.line`);
      assert.equal(lines.length, 1, `${id}: one line path`);
      assert.ok(p.$$(`#${id} path`).length <= 2, `${id}: a line and at most its wash`);
      assert.ok(p.$(`#${id} .plotwrap`).getAttribute('aria-label').includes(':'), `${id} is described`);
      assert.equal(p.$$(`#${id} .xaxis span`).length, 4, `${id}: four time ticks`);
    }
    assert.equal(p.calls.filter((c) => c.url.endsWith('/v1/admin/metrics/history')).length, 1);
    assert.equal(p.text('#chHit .latest'), '90 %');
    assert.equal(p.text('#chArchive .latest'), '231K', 'the newest point is the live snapshot');
    // A single series needs no legend; colour never carries identity alone.
    assert.equal(p.$('#chArchive .legend'), null);
  } finally { p.close(); }
});

test('points from before ADM-04 have no disk: the disk line starts later, as a gap, without errors', async () => {
  const p = await page({ key: KEY });
  try {
    const d = p.$('#chDisk path.line').getAttribute('d');
    assert.ok(d.startsWith('M'), d.slice(0, 20));
    const firstX = Number(d.slice(1).split(',')[0]);
    assert.ok(firstX > 30, `the line starts where disk readings start (x=${firstX})`);
    assert.equal((p.$('#chArchive path.line').getAttribute('d').match(/M/g) ?? []).length, 1, 'an unbroken series is one subpath');
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('a snapshot extends the charts without refetching history', async () => {
  const p = await page({ key: KEY });
  try {
    p.push('metrics.snapshot', snapshot({ totals: { ...scene.snapshot.totals, archivedMatches: 250_000 } }));
    await p.settle();
    assert.equal(p.text('#chArchive .latest'), '250K');
    assert.equal(p.calls.filter((c) => c.url.endsWith('/v1/admin/metrics/history')).length, 1);
  } finally { p.close(); }
});

test('the readout follows the arrow keys; the table toggle shows the numbers', async () => {
  const p = await page({ key: KEY });
  try {
    const wrap = p.$('#chBacklog .plotwrap');
    assert.equal(wrap.tabIndex, 0);
    wrap.dispatchEvent(new p.w.KeyboardEvent('keydown', { key: 'End', bubbles: true }));
    const readout = p.$('#chBacklog .readout');
    assert.equal(readout.hidden, false);
    assert.match(readout.textContent, /^\d\d:\d\d · /);
    wrap.dispatchEvent(new p.w.KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
    assert.match(readout.textContent, /· 1\.2M$/, readout.textContent);
    wrap.dispatchEvent(new p.w.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    assert.equal(readout.hidden, true);

    const btn = p.$('#chBacklog [data-table]');
    assert.equal(btn.getAttribute('aria-expanded'), 'false');
    btn.click();
    assert.equal(btn.getAttribute('aria-expanded'), 'true');
    const rows = p.$$('#chBacklog .tableview tbody tr');
    assert.equal(rows.length, 200, 'capped');
    assert.match(p.text('#chBacklog .more-rows'), /^showing 200 of \d,\d{3}$/);
  } finally { p.close(); }
});

test('an open table stays open when new data redraws the chart, and a closed one stays closed', async () => {
  const p = await page({ key: KEY });
  try {
    p.$('#chBacklog [data-table]').click();
    p.push('metrics.snapshot', snapshot());
    await p.settle();
    const btn = p.$('#chBacklog [data-table]');
    assert.equal(btn.getAttribute('aria-expanded'), 'true', 'still open after the redraw');
    assert.equal(p.$('#chBacklog .tableview').hidden, false);
    assert.equal(p.$$('#chBacklog .tableview tbody tr').length, 200);
    assert.equal(p.$('#chArchive [data-table]').getAttribute('aria-expanded'), 'false', 'only the one opened');

    btn.click();
    p.push('metrics.snapshot', snapshot());
    await p.settle();
    assert.equal(p.$('#chBacklog [data-table]').getAttribute('aria-expanded'), 'false', 'closed stays closed');
    assert.equal(p.$('#chBacklog .tableview').hidden, true);
  } finally { p.close(); }
});

test('recent errors on entry, refreshed when a snapshot counts new 4xx or 5xx', async () => {
  const p = await page({ key: KEY });
  try {
    const rows = p.$$('#errors tbody tr');
    assert.equal(rows.length, 2);
    assert.ok(rows[0].textContent.includes('UPSTREAM_ERROR'));
    const errorCalls = () => p.calls.filter((c) => c.url.includes('kind=errors')).length;
    const before = errorCalls();
    p.push('metrics.snapshot', snapshot());
    await p.settle();
    assert.equal(errorCalls(), before, 'the first snapshot sets the count');
    p.push('metrics.snapshot', snapshot({ requests: { '/v1/lol/x|200|hit': 100, '/v1/lol/x|502|none': 3 } }));
    await p.settle();
    assert.equal(errorCalls(), before + 1, 'one more 502: the panel refreshes');
  } finally { p.close(); }
});

test('with too little history a chart says so instead of drawing', async () => {
  const saved = scene.history;
  scene.history = { intervalS: 60, maxPoints: 1440, points: [] };
  const p = await page({ key: KEY });
  try {
    assert.equal(p.text('#chDisk .nodata'), 'Not enough history yet.');
    assert.equal(p.$('#chDisk [data-table]'), null);
  } finally { p.close(); scene.history = saved; }
});

test('coming back to the overview refetches history and keeps the live snapshot as its end', async () => {
  const p = await page({ key: KEY });
  try {
    p.w.location.hash = '#traffic';
    await p.settle();
    p.w.location.hash = '#overview';
    await p.settle();
    assert.equal(p.calls.filter((c) => c.url.endsWith('/v1/admin/metrics/history')).length, 2, 'once per entry');
    assert.equal(p.text('#chArchive .latest'), '231K');
  } finally { p.close(); }
});

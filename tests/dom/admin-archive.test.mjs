// The Archive & Storage tab (ADM-13) in jsdom: archive and storage tiles, the
// data disk as a stacked bar with a text legend, warnings at 85 % and 95 %
// used shown without colour, the backups with a stale one flagged, the last
// maintenance run and Run now, the timeline backfill, and the analytics
// actions, each behind a confirm.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot, ladderSnapshot } from './fake-admin.mjs';

const H = 3_600_000;
beforeEach(() => {
  scene.snapshot = ladderSnapshot();
  scene.posted = [];
  scene.backups = { dir: '/data/backups', keep: 14, files: [{ name: 'riot-proxy-2026-10-13.db', bytes: 21 * 1024 ** 3, modifiedAt: Date.now() - 7 * H }], lastRun: null, diskFreeBytes: 51 * 1024 ** 3 };
});

test('tiles, the disk as a stacked bar with every part named, and when the bytes were sampled', async () => {
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    const tiles = p.$$('#arTiles .tile').map((t) => [...t.children].map((c) => c.textContent.trim()).join(' '));
    assert.deepEqual(tiles, [
      'Archived matches 231K 216K timelines',
      'Stored 2.3 GB 14.9 GB raw · 6.5× zstd',
      'Players 41.2K 3 tracked',
      'Database 22.0 GB 0 B free pages · WAL 0 B',
      'Disk free 51.0 GB 56 % used of 115 GB',
    ]);
    const bar = p.$('#arDisk .stack');
    assert.equal(bar.getAttribute('role'), 'img');
    assert.equal(bar.getAttribute('aria-label'), 'live pages 22.0 GB, free pages 0 B, WAL 0 B, other files 42.0 GB, free 51.0 GB');
    assert.equal(p.words('#arDisk .legend'), 'live pages 22.0 GB free pages 0 B WAL 0 B other files 42.0 GB free 51.0 GB');
    assert.equal(p.$('#arDisk .note-line'), null, 'no warning at 56 %');
    assert.equal(p.text('#arSampled'), 'archive bytes sampled just now');
  } finally { p.close(); }
});

test('under 5 % free: a warning with an icon here and in the health strip', async () => {
  scene.snapshot = snapshot({ disk: { dbBytes: 100, walBytes: 0, freelistBytes: 0, freeBytes: 4, totalBytes: 100 } });
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    const warn = p.$('#arDisk .note-line');
    assert.ok(warn.querySelector('use[href="#i-critical"]'));
    assert.match(warn.textContent, /^ Under 5 % of the disk is free/);
    assert.equal(p.$('#sDisk').dataset.level, 'critical');
    assert.ok(p.$('#sDisk use[href="#i-critical"]'), 'the strip says it with an icon');
    assert.equal(p.text('#sDisk'), 'disk 96 % · 4 B free', 'and in words');
  } finally { p.close(); }
});

test('backups: listed with sizes; one older than 36 h is flagged; the last run is reported', async () => {
  scene.backups = {
    dir: '/data/backups', keep: 14, diskFreeBytes: 51 * 1024 ** 3,
    files: [{ name: 'riot-proxy-2026-10-11.db', bytes: 21 * 1024 ** 3, modifiedAt: Date.now() - 40 * H }],
    lastRun: { at: Date.now() - 2 * H, ms: 1500, error: null, report: { backup: null, backupSkipped: '1 bytes free, the copy needs 9', backupFailed: null, backupsDeleted: 0, jobsDeleted: 12, historyDeleted: 3, cacheSwept: 4 } },
  };
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    const flag = p.$('#arBackups .note-line');
    assert.ok(flag.querySelector('use[href="#i-warning"]'), 'flagged with an icon');
    assert.equal(p.words('#arBackups .note-line'), 'backup 1d ago: a night was missed · keeping 14 · 51.0 GB free beside them');
    assert.match(p.text('#arBackups p.muted'), /^Last maintenance 2h ago, took 1\.5 s: backup skipped \(1 bytes free, the copy needs 9\); 12 done jobs and 3 history points trimmed, 4 cache rows swept\.$/);
    assert.deepEqual(p.$$('#arBackups tbody tr').map((tr) => [...tr.children].map((c) => c.textContent)), [['riot-proxy-2026-10-11.db', '21.0 GB', '2d ago']]);
  } finally { p.close(); }
});

test('a fresh backup is not flagged; keep 0 says so', async () => {
  let p = await page({ key: KEY, hash: '#archive' });
  try {
    assert.ok(p.$('#arBackups .note-line use[href="#i-good"]'));
    assert.equal(p.words('#arBackups .note-line'), 'backup 7h ago · keeping 14 · 51.0 GB free beside them');
  } finally { p.close(); }
  scene.backups = { dir: '/data/backups', keep: 0, files: [], lastRun: null, diskFreeBytes: 1 };
  p = await page({ key: KEY, hash: '#archive' });
  try {
    assert.match(p.words('#arBackups'), /^backups off · keeping 0/);
    assert.equal(p.text('#arBackups tbody'), 'BACKUP_KEEP=0: no backups are kept on this disk.');
  } finally { p.close(); }
});

test('Run maintenance now asks, queues and says so', async () => {
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    p.$('#arRun').click();
    await p.settle();
    assert.match(p.text('#confirmText'), /^Run maintenance now\? It backs up only if today/);
    p.$('#confirmOk').click();
    await p.settle();
    assert.deepEqual(scene.posted, ['/v1/admin/maintenance/run']);
    assert.equal(p.text('#toasts'), 'Maintenance queued.');
  } finally { p.close(); }
});

test('the timeline backfill: a meter with its numbers, what is left per region', async () => {
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    assert.equal(p.text('#arTlMeta'), 'newest 2 patch(es)');
    assert.equal(p.$('#arTimelines .meter').getAttribute('aria-valuetext'), '600 of 880');
    assert.equal(p.text('#arTimelines p'), '900 ranked matches · 280 left · 20 Riot does not have');
    assert.deepEqual(p.$$('#arTimelines li').map((li) => [...li.children].map((c) => c.textContent).join(' ')), ['sea 200 left 16.20', 'asia 80 left 16.20']);
  } finally { p.close(); }
});

test('analytics actions: recompute the newest ladder, re-extract, sync Data Dragon, fill names, each after a confirm', async () => {
  const p = await page({ key: KEY, hash: '#archive' });
  try {
    assert.match(p.words('#arRuns'), /oc1 RANKED_SOLO_5x5 10m ago/);
    assert.equal(p.text('#arTop'), 'Most played, newest patch: Ahri 6g 50%');
    const click = async (act, expect) => {
      p.$(`#arActions [data-act="${act}"]`).click();
      await p.settle();
      p.$('#confirmOk').click();
      await p.settle();
      assert.equal(p.$$('#toasts .toast').at(-1).textContent, expect);
    };
    await click('reextract', 'Re-extract queued: 1,234 matches to read.');
    await click('ddragon', 'Data Dragon sync queued.');
    await click('names', 'Names backfill queued: 17 players without a name.');
    await click('recompute', 'Recompute queued.');
    assert.deepEqual(scene.posted, ['/v1/admin/analytics/reextract', '/v1/admin/ddragon/sync', '/v1/admin/players/names/backfill']);
    assert.deepEqual(scene.ladder.bodies.at(-1), ['/v1/admin/analytics/recompute', { platform: 'oc1', queue: 'RANKED_SOLO_5x5' }]);
    // Declined: nothing is posted.
    p.$('#arActions [data-act="ddragon"]').click();
    await p.settle();
    p.$('#confirmCancel').click();
    await p.settle();
    assert.equal(scene.posted.length, 3);
  } finally { p.close(); }
});

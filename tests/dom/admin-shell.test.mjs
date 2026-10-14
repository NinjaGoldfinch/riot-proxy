// The dashboard's shell (ADM-08) in jsdom: the key sheet and its probe
// (GET /v1/admin/info), the hash router, the theme, the store fanning a
// metrics.snapshot frame out to the strip and the tiles, the metrics topic
// dropped while the tab is hidden, and one base URL for fetch and the socket.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot } from './fake-admin.mjs';

test('without a key the probe is refused and the key sheet asks for one', async () => {
  const p = await page();
  try {
    assert.equal(p.$('#sheet').hidden, false, 'the sheet is open');
    assert.equal(p.calls[0].url, 'http://admin.test/v1/admin/info', 'the probe is /v1/admin/info');
    assert.ok(!p.calls.some((c) => c.url.includes('/dashboard/config.json')));
    assert.equal(p.sockets.length, 0, 'no socket before a key');
    p.$('#keyInput').value = KEY;
    p.$('#sheetForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    assert.equal(p.$('#sheet').hidden, true, 'an accepted key closes the sheet');
    assert.equal(p.w.localStorage.getItem('rp.dashboard.key'), KEY, 'and is kept in this browser');
    assert.ok(p.calls.filter((c) => c.url.endsWith('/v1/admin/info')).at(-1).auth === `Bearer ${KEY}`);
    assert.equal(p.text('#keyLabel'), 'rpx_…01');
    assert.equal(p.sockets.length, 1);
    assert.deepEqual(p.errors, []);
  } finally { p.close(); }
});

test('a refused key, one without the admin scope, or a refused address reopens the sheet with the server\'s reason', async () => {
  for (const [key, why] of [
    ['rpx_wrong', 'Missing or invalid API key'],
    ['rpx_reader', "This key lacks the 'admin' scope"],
    ['rpx_far', 'Admin access is not permitted from this address'],
  ]) {
    const p = await page({ key });
    try {
      assert.equal(p.$('#sheet').hidden, false, key);
      assert.equal(p.$('#sheetError').hidden, false);
      assert.equal(p.text('#sheetError'), why);
    } finally { p.close(); }
  }
});

test('with AUTH_DISABLED the probe answers without a key and nothing asks for one', async () => {
  scene.authDisabled = true;
  const p = await page();
  try {
    assert.equal(p.$('#sheet').hidden, true);
    assert.equal(p.sockets.length, 1);
    assert.equal(p.socket().url, 'ws://admin.test/v1/ws', 'no token on the socket');
  } finally { p.close(); scene.authDisabled = false; }
});

test('the router: the overview by default, an unknown hash falls back, tabs are a tablist', async () => {
  const p = await page({ key: KEY, hash: '#nowhere/at/all' });
  try {
    const tab = p.$('#tab-overview');
    assert.equal(p.$('#tabs').getAttribute('role'), 'tablist');
    assert.equal(tab.getAttribute('role'), 'tab');
    assert.equal(tab.getAttribute('aria-selected'), 'true');
    assert.equal(tab.getAttribute('aria-controls'), 'panel-overview');
    assert.equal(p.$('#panel-overview').hidden, false);
    assert.equal(p.$('#panel-overview').getAttribute('role'), 'tabpanel');
    assert.equal(p.w.rpAdmin.route('#overview/x').id, 'x');
    tab.click();
    assert.equal(p.w.location.hash, '#overview');
  } finally { p.close(); }
});

test('the first snapshot fills the health strip and the tiles', async () => {
  const p = await page({ key: KEY });
  try {
    assert.equal(p.text('#sReady'), 'ready');
    assert.equal(p.$('#sReady').dataset.level, 'good');
    assert.equal(p.text('#sLimiter'), 'limiter ok');
    assert.equal(p.text('#sDisk'), 'disk 56 % · 51.0 GB free');
    assert.equal(p.text('#sBackup'), 'backup 7h ago');
    assert.equal(p.text('#sScope'), 'scope 5f3a9c1e');
    assert.match(p.text('#sBuild'), /^v2\.3\.0 · 41567bd · production · up 3d 4h$/);
    const tiles = p.$$('#tiles .tile').map((t) => [...t.children].map((c) => c.textContent.trim()).join(' ').replace(/\s+/g, ' '));
    assert.equal(tiles.length, 7);
    assert.ok(tiles[0].startsWith('Archived matches 231K'), tiles[0]);
    assert.ok(tiles[1].startsWith('Jobs pending 1.13M 3 running · 12 failed in 24 h'), tiles[1]);
    assert.ok(tiles[2].startsWith('Cache hit ratio 95.0 %'), tiles[2]);
    assert.ok(tiles[5].startsWith('Disk free 51.0 GB 56 % used'), tiles[5]);
    assert.ok(tiles[6].includes('6/6 readers free'), tiles[6]);
    // Each status carries an icon and words, never colour alone.
    for (const id of ['#sReady', '#sLimiter', '#sDisk', '#sBackup']) {
      assert.ok(p.$(`${id} svg use`), id);
      assert.ok(p.text(id).length > 3, id);
    }
  } finally { p.close(); }
});

test('a metrics.snapshot frame fans out to every section that listens', async () => {
  const p = await page({ key: KEY });
  try {
    const seen = [];
    p.w.rpAdmin.store.on('snapshot', (s) => seen.push(s.totals.archivedMatches));
    const next = snapshot({
      totals: { ...scene.snapshot.totals, archivedMatches: 231_915 },
      limiter: [{ scope: 'oc1', kind: 'platform', label: 'Oceania', frozenMs: 9000, windows: [], methods: [], interactiveWaiters: 0 }],
      disk: { ...scene.snapshot.disk, freeBytes: 4 * 1024 ** 3 },
      requests: { '/v1/lol/x|200|hit': 150, '/v1/lol/x|502|none': 2 },
    });
    p.push('metrics.snapshot', next);
    await p.settle();
    assert.deepEqual(seen, [231_915]);
    assert.match(p.text('#tiles .tile:first-child'), /\+1,200 since this page opened/);
    assert.equal(p.text('#sLimiter'), 'limiter 1 frozen');
    assert.equal(p.$('#sLimiter').dataset.level, 'serious');
    assert.equal(p.$('#sDisk').dataset.level, 'critical', 'under 5 % free');
    assert.ok(p.text('#sDisk').startsWith('disk 97 %'));
    assert.match(p.$$('#tiles .tile')[3].textContent, /\d+(\.\d+)? \/s/, 'a request rate from two snapshots');
  } finally { p.close(); }
});

test('the socket subscribes to metrics and firehose; hidden drops metrics, visible takes it back and refetches', async () => {
  const p = await page({ key: KEY });
  try {
    assert.equal(p.socket().url, `ws://admin.test/v1/ws?token=${KEY}`);
    assert.deepEqual(p.socket().sent[0], { op: 'subscribe', topics: ['metrics', 'firehose'] });
    assert.equal(p.text('#conn'), 'live');
    await p.setHidden(true);
    assert.deepEqual(p.socket().sent.at(-1), { op: 'unsubscribe', topics: ['metrics'] });
    assert.equal(p.text('#conn'), 'paused');
    const before = p.calls.filter((c) => c.url.endsWith('/v1/admin/metrics')).length;
    await p.setHidden(false);
    assert.deepEqual(p.socket().sent.at(-1), { op: 'subscribe', topics: ['metrics'] });
    assert.equal(p.calls.filter((c) => c.url.endsWith('/v1/admin/metrics')).length, before + 1, 'the snapshot is refetched');
    assert.equal(p.text('#conn'), 'live');
  } finally { p.close(); }
});

test('a dropped socket backs off; a 4401 close asks for a key instead', async () => {
  const p = await page({ key: KEY });
  try {
    p.socket().drop(1006);
    await p.settle();
    assert.equal(p.text('#conn'), 'reconnecting in 1s');
    await new Promise((r) => setTimeout(r, 1100));
    await p.settle();
    assert.equal(p.sockets.length, 2, 'reconnected after a second');
    p.socket().drop(4401, 'key revoked');
    await p.settle();
    assert.equal(p.$('#sheet').hidden, false);
    assert.equal(p.text('#sheetError'), 'key revoked');
  } finally { p.close(); }
});

test('no polling: the only timer is the one-second clock', async () => {
  const p = await page({ key: KEY });
  try {
    assert.deepEqual(p.intervals, [1000]);
  } finally { p.close(); }
});

test('the theme follows the system until chosen, then the choice is kept', async () => {
  let p = await page({ key: KEY, prefersLight: true });
  try {
    assert.equal(p.w.document.documentElement.dataset.theme, 'light');
    p.$('#theme').click();
    assert.equal(p.w.document.documentElement.dataset.theme, undefined, 'dark');
    assert.equal(p.w.localStorage.getItem('rp.admin.theme'), 'dark');
    assert.equal(p.$('#theme').getAttribute('aria-label'), 'Switch to the light theme');
  } finally { p.close(); }
  p = await page({ key: KEY, theme: 'light' });
  try {
    assert.equal(p.w.document.documentElement.dataset.theme, 'light', 'a saved choice wins over the system');
  } finally { p.close(); }
});

test('the base URL setting re-points every fetch and the socket', async () => {
  const p = await page({ key: KEY, base: 'https://api.example' });
  try {
    const urls = p.calls.map((c) => c.url);
    assert.ok(urls.length >= 4);
    assert.ok(urls.every((u) => u.startsWith('https://api.example/')), urls.join(' '));
    assert.equal(p.socket().url, `wss://api.example/v1/ws?token=${KEY}`);
    assert.equal(p.text('#baseLabel'), 'api.example');
  } finally { p.close(); }
});

test('sign out forgets the key and asks again', async () => {
  const p = await page({ key: KEY });
  try {
    p.$('#signOut').click();
    await p.settle();
    assert.equal(p.w.localStorage.getItem('rp.dashboard.key'), null);
    assert.equal(p.$('#sheet').hidden, false);
    assert.equal(p.socket().readyState, 3, 'the socket is closed');
  } finally { p.close(); }
});

test('confirmAction resolves with the choice; toasts are announced', async () => {
  const p = await page({ key: KEY });
  try {
    const yes = p.w.rpAdmin.confirmAction('Delete 3 rows?', { confirm: 'Delete', danger: true });
    assert.equal(p.$('#confirm').hidden, false);
    assert.equal(p.text('#confirmText'), 'Delete 3 rows?');
    assert.equal(p.text('#confirmOk'), 'Delete');
    p.$('#confirmOk').click();
    assert.equal(await yes, true);
    assert.equal(p.$('#confirm').hidden, true);
    const no = p.w.rpAdmin.confirmAction('Again?');
    p.$('#confirm').dispatchEvent(new p.w.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    assert.equal(await no, false);
    p.w.rpAdmin.toast('Queued.');
    assert.equal(p.$('#toasts').getAttribute('aria-live'), 'polite');
    assert.equal(p.text('#toasts'), 'Queued.');
  } finally { p.close(); }
});

test('the table helper sorts on a header click and caps its rows', async () => {
  const p = await page({ key: KEY });
  try {
    const rows = Array.from({ length: 5 }, (_, i) => ({ name: `r${i}`, n: (i * 7) % 5 }));
    const el = p.w.rpAdmin.table({ columns: [{ key: 'name', label: 'Name' }, { key: 'n', label: 'N', num: true }], rows, cap: 3, sort: { key: 'n', dir: 'desc' } });
    p.w.document.body.append(el);
    const col = () => [...el.querySelectorAll('tbody tr')].map((tr) => tr.lastElementChild.textContent);
    assert.deepEqual(col(), ['4', '3', '2']);
    assert.equal(el.querySelector('.more-rows').textContent, 'showing 3 of 5');
    assert.equal(el.querySelector('th[aria-sort="descending"]').textContent.trim(), 'N ▼');
    el.querySelector('[data-sort="n"]').click();
    assert.deepEqual(col(), ['0', '1', '2']);
    assert.equal(el.querySelector('th[aria-sort="ascending"]').textContent.trim(), 'N ▲');
    el.querySelector('[data-sort="name"]').click();
    assert.equal(el.querySelector('tbody tr td').textContent, 'r4');
  } finally { p.close(); }
});

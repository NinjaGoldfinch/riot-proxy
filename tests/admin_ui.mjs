// Unit tests for the dashboard's pure helpers (ADM-08) and its theme tokens.
// The helpers live in one marked block of src/ui/admin.html, which has no build
// step, so this test cuts that block out and evaluates it; the tokens are read
// from the page's two token blocks. Run by `cargo test` (tests/ui.rs) when node
// is on PATH:
//   node --test tests/admin_ui.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../src/ui/admin.html', import.meta.url), 'utf8');
const block = html.match(/\/\/ -{10} pure helpers[^\n]*\n([\s\S]*?)\/\/ -{10} end pure helpers/);
assert.ok(block, 'the pure helpers block is marked in admin.html');
const h = new Function(`${block[1]}; return { APEX_LIST_CAP, fmt, compact, fmtBytes, fmtDuration, pct, esc, relTime, sumValues, hitRatio, rate, jobTotals, diskLevel, backupLevel, limiterLevel, route, joinUrl, wsUrl, maskKey, contrast, sortRows, capRows, percentile, downsample, historyPointOf, seriesOf, statusClass, routeRows, upstreamRows };`)();

const tokens = (name) => {
  const m = html.match(new RegExp(`/\\* tokens:${name}[^*]*\\*/\\s*[^{]+\\{([^}]*)\\}`));
  assert.ok(m, `token block ${name}`);
  return Object.fromEntries([...m[1].matchAll(/--([\w-]+):\s*([^;]+);/g)].map(([, k, v]) => [k, v.trim()]));
};

test('numbers: grouped, compact for tiles, bytes in binary units', () => {
  assert.equal(h.fmt(1234567), '1,234,567');
  assert.equal(h.fmt(null), '—');
  assert.deepEqual([0, 1284, 9999, 12_900, 230_715, 1_126_303, 2_100_000_000].map(h.compact), ['0', '1,284', '9,999', '12.9K', '231K', '1.13M', '2.1B']);
  assert.equal(h.compact(-12_900), '-12.9K');
  assert.deepEqual([0, 1023, 1536, 51 * 1024 ** 3, 2.5 * 1024 ** 4].map(h.fmtBytes), ['0 B', '1023 B', '1.5 KB', '51.0 GB', '2.5 TB']);
  assert.equal(h.pct(0.9412), '94.1 %');
  assert.equal(h.pct(null), '—');
});

test('durations and ages read as an operator says them', () => {
  assert.equal(h.fmtDuration(65), '1m 5s');
  assert.equal(h.fmtDuration(3 * 3600 + 120), '3h 2m');
  assert.equal(h.fmtDuration(3 * 86400 + 4 * 3600), '3d 4h');
  assert.equal(h.fmtDuration(-1), '—');
  const now = 1_800_000_000_000;
  assert.equal(h.relTime(now - 30_000, now), 'just now');
  assert.equal(h.relTime(now - 20 * 60_000, now), '20m ago');
  assert.equal(h.relTime(new Date(now - 7 * 3_600_000).toISOString(), now), '7h ago');
  assert.equal(h.relTime(now - 3 * 86_400_000, now), '3d ago');
  assert.equal(h.relTime(null, now), '—');
});

test('markup is escaped', () => {
  assert.equal(h.esc(`<img src=x onerror="a('b')">&`), '&lt;img src=x onerror=&quot;a(&#39;b&#39;)&quot;&gt;&amp;');
  assert.equal(h.esc(null), '');
});

test('the hit ratio is the alert rule: hit, neg and stale over every read', () => {
  assert.equal(h.hitRatio({ hit: 80, neg: 5, stale: 5, miss: 10 }), 0.9);
  assert.equal(h.hitRatio({ hit: 0, neg: 0, stale: 0, miss: 0 }), null);
  assert.equal(h.hitRatio(null), null);
});

test('rates need two readings and treat a drop as a restart', () => {
  assert.equal(h.rate({ at: 0, total: 100 }, { at: 5000, total: 150 }), 10);
  assert.equal(h.rate(null, { at: 5000, total: 150 }), null);
  assert.equal(h.rate({ at: 0, total: 100 }, { at: 5000, total: 20 }), null, 'counters reset');
  assert.equal(h.rate({ at: 5000, total: 1 }, { at: 5000, total: 2 }), null);
  assert.equal(h.sumValues({ a: 2, b: 3 }), 5);
});

test('job totals sum the queues: pending is waiting plus delayed', () => {
  assert.deepEqual(h.jobTotals({ a: { active: 1, waiting: 10, delayed: 2, failed: 3 }, b: { active: 2, waiting: 0, delayed: 0, failed: 0 } }), { running: 3, pending: 12, failed: 3 });
  assert.deepEqual(h.jobTotals(undefined), { running: 0, pending: 0, failed: 0 });
});

test('disk warns at 85 % used and is critical at 95 %', () => {
  const d = (free) => h.diskLevel({ freeBytes: free, totalBytes: 100 });
  assert.deepEqual([d(50).level, d(15).level, d(14).level, d(5).level, d(4).level], ['good', 'warning', 'warning', 'critical', 'critical']);
  assert.equal(h.diskLevel(null).level, 'unknown');
});

test('a backup older than 36 h is flagged; none is serious; keep 0 says so', () => {
  const now = 1_800_000_000_000;
  const at = (hours) => ({ keep: 14, files: [{ modifiedAt: now - hours * 3_600_000 }] });
  assert.deepEqual(h.backupLevel(at(7), now), { level: 'good', text: 'backup 7h ago' });
  assert.equal(h.backupLevel(at(37), now).level, 'warning');
  assert.equal(h.backupLevel({ keep: 14, files: [] }, now).level, 'serious');
  assert.deepEqual(h.backupLevel({ keep: 0, files: [] }, now), { level: 'warning', text: 'backups off' });
  assert.equal(h.backupLevel(null, now).level, 'unknown');
});

test('the limiter reads frozen scopes before held ones', () => {
  assert.equal(h.limiterLevel({ limiter: [{ frozenMs: 5 }, { frozenMs: 0 }], gates: { blockedScopes: [1] } }).text, 'limiter 1 frozen');
  assert.equal(h.limiterLevel({ limiter: [], gates: { blockedScopes: [1, 2] } }).text, 'bulk held on 2');
  assert.equal(h.limiterLevel({ limiter: [], gates: { blockedScopes: [] } }).level, 'good');
  assert.equal(h.limiterLevel(null).level, 'unknown');
});

test('the hash router: a tab and an id, anything unknown is the first tab', () => {
  const tabs = ['overview', 'jobs'];
  assert.deepEqual(h.route('#jobs/01J9ABC', tabs), { tab: 'jobs', id: '01J9ABC' });
  assert.deepEqual(h.route('#jobs', tabs), { tab: 'jobs', id: null });
  assert.deepEqual(h.route('#jobs/a%2Fb', tabs), { tab: 'jobs', id: 'a/b' });
  assert.deepEqual(h.route('#nope', tabs), { tab: 'overview', id: null });
  assert.deepEqual(h.route('', tabs), { tab: 'overview', id: null });
});

test('one base URL for fetch and the socket', () => {
  assert.equal(h.joinUrl('https://admin.example/', '/v1/admin/info'), 'https://admin.example/v1/admin/info');
  assert.equal(h.wsUrl('https://admin.example', 'rpx_a b'), 'wss://admin.example/v1/ws?token=rpx_a%20b');
  assert.equal(h.wsUrl('http://localhost:8080', ''), 'ws://localhost:8080/v1/ws');
  assert.equal(h.maskKey('rpx_abcdefghij'), 'rpx_…ij');
  assert.equal(h.maskKey(''), '—');
});

test('tables sort either way, empty values last, and cap their rows', () => {
  const rows = [{ n: 2, s: 'b' }, { n: null, s: 'c' }, { n: 10, s: 'a' }];
  assert.deepEqual(h.sortRows(rows, 'n').map((r) => r.n), [10, 2, null]);
  assert.deepEqual(h.sortRows(rows, 'n', 'asc').map((r) => r.n), [2, 10, null]);
  assert.deepEqual(h.sortRows(rows, 's', 'asc').map((r) => r.s), ['a', 'b', 'c']);
  assert.deepEqual(h.sortRows(rows, (r) => r.s.charCodeAt(0)).map((r) => r.s), ['c', 'b', 'a']);
  assert.deepEqual(h.capRows([1, 2, 3], 2), { shown: [1, 2], more: 1 });
  assert.deepEqual(h.capRows([1], 2), { shown: [1], more: 0 });
});

test("the apex cap is Riot's 10,000 (tests/ui.rs checks it against the server)", () => {
  assert.equal(h.APEX_LIST_CAP, 10000);
});

test('text tokens meet WCAG AA (4.5:1) on every surface, in both themes', () => {
  for (const theme of ['dark', 'light']) {
    const t = tokens(theme);
    for (const ink of ['ink', 'ink-2', 'muted']) {
      for (const surface of ['page', 'panel', 'panel-2']) {
        const c = h.contrast(t[ink], t[surface]);
        assert.ok(c >= 4.5, `${theme} --${ink} ${t[ink]} on --${surface} ${t[surface]}: ${c.toFixed(2)}`);
      }
    }
  }
});

test('status icons are at least 3:1 on the panels, and never a series colour', () => {
  for (const theme of ['dark', 'light']) {
    const t = tokens(theme);
    const series = ['s1', 's2', 's3', 's4', 's5', 's6'].map((s) => t[s].toLowerCase());
    for (const s of ['good', 'warning', 'serious', 'critical']) {
      for (const surface of ['page', 'panel']) {
        const c = h.contrast(t[s], t[surface]);
        assert.ok(c >= 3, `${theme} --${s} ${t[s]} on --${surface}: ${c.toFixed(2)}`);
      }
      assert.ok(!series.includes(t[s].toLowerCase()), `${theme} --${s} reuses a series colour`);
    }
  }
});

test('the series order is design/13 §Visual tokens, as validated', () => {
  assert.deepEqual(['s1', 's2', 's3', 's4', 's5', 's6'].map((s) => tokens('dark')[s]), ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181', '#9085e9']);
  assert.deepEqual(['s1', 's2', 's3', 's4', 's5', 's6'].map((s) => tokens('light')[s]), ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#4a3aa7']);
});

test('percentiles are nearest-rank and skip nulls', () => {
  const v = [5, 1, 4, 2, 3, null];
  assert.equal(h.percentile(v, 50), 3);
  assert.equal(h.percentile(v, 95), 5);
  assert.equal(h.percentile([7], 95), 7);
  assert.equal(h.percentile([], 50), null);
});

test('downsampling keeps at most n points, ends at the last time, and keeps gaps', () => {
  const pts = Array.from({ length: 1440 }, (_, i) => [i, i < 100 ? null : i]);
  const d = h.downsample(pts, 600);
  assert.equal(d.length, 600);
  assert.equal(d.at(-1)[0], 1439);
  assert.equal(d[0][1], null, 'a bucket of nulls stays a gap');
  assert.deepEqual(h.downsample(pts.slice(0, 3), 600), pts.slice(0, 3));
});

test('a snapshot becomes a history point', () => {
  const s = { totals: { archivedMatches: 5 }, queues: { a: { active: 1, waiting: 2, delayed: 3, failed: 4 } }, cache: { hit: 1 }, disk: { freeBytes: 9 }, requests: { a: 2, b: 3 }, upstream: { c: 7 } };
  assert.deepEqual(h.historyPointOf(s, 10), { t: 10, totals: { archivedMatches: 5 }, queues: { active: 1, pending: 5, failed: 4 }, cache: { hit: 1 }, disk: { freeBytes: 9 }, requests: 5, upstream: 7 });
  assert.equal(h.historyPointOf({ totals: {}, queues: {} }, 1).disk, null);
});

test('series: values, and rates and hit ratios between neighbours, null across restarts and gaps', () => {
  const pt = (t, hit, miss, req, disk) => ({ t: t * 60_000, totals: { archivedMatches: t }, queues: { pending: 10 - t }, cache: { hit, miss, neg: 0, stale: 0 }, requests: req, upstream: req, ...(disk == null ? {} : { disk: { freeBytes: disk } }) });
  const v = h.seriesOf([pt(0, 0, 0, null, null), pt(1, 90, 10, 600, 5), pt(2, 180, 20, 1200, 4), pt(3, 5, 5, 60, 4)]);
  assert.deepEqual(v.archive.map((p) => p[1]), [0, 1, 2, 3]);
  assert.deepEqual(v.backlog.map((p) => p[1]), [10, 9, 8, 7]);
  assert.deepEqual(v.disk.map((p) => p[1]), [null, 5, 4, 4], 'a point without disk is a gap');
  assert.deepEqual(v.hit.map((p) => p[1]), [null, 0.9, 0.9, null], 'the counters reset at 3: a restart');
  assert.deepEqual(v.requests.map((p) => p[1]), [null, null, 10, null], 'per second; no rate across a gap or a restart');
});

test('status classes and the traffic tables', () => {
  assert.equal(h.statusClass(503), '5xx');
  assert.equal(h.statusClass('200'), '2xx');
  const rows = h.routeRows(
    { '/a|200|hit': 8, '/a|200|miss': 2, '/a|500|none': 1, '/b|404|neg': 3 },
    [{ route: '/a', latencyMs: 10 }, { route: '/a', latencyMs: 30 }, { route: '/c', latencyMs: 99 }],
  );
  const a = rows.find((r) => r.route === '/a');
  assert.deepEqual([a.total, a.s2xx, a.s5xx, a.hitRatio, a.p50, a.p95, a.sampled], [11, 10, 1, 0.8, 10, 30, 2]);
  const b = rows.find((r) => r.route === '/b');
  assert.deepEqual([b.total, b.s4xx, b.hitRatio, b.p50], [3, 3, 1, null]);
  assert.deepEqual(h.upstreamRows({ 'sea|match.byId|200': 4 }), [{ region: 'sea', method: 'match.byId', status: '200', count: 4 }]);
  assert.deepEqual(h.routeRows(undefined), []);
});

// The fake admin API both dashboard suites drive the page against: the jsdom
// tests (dashboard.test.mjs) and the browser layout check (dashboard.browser.test.mjs).
// Shapes are the OpenAPI document's for the crawl list, a crawl's activity, the
// job queue, the ladder options and the analytics recompute.
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../../src/ui/dashboard.html', import.meta.url), 'utf8');
const ago = (ms) => new Date(Date.now() - ms).toISOString();
const soon = (ms) => new Date(Date.now() + ms).toISOString();

const RUNNING = '01K0000000000000000000RUN1';
const DONE = '01K0000000000000000000DON1';
const crawl = (id, status, phase, extra = {}) => ({
  id, platform: 'oc1', queue: 'RANKED_SOLO_5x5', tierFloor: 'MASTER', status, phase,
  startedAt: ago(3_600_000), finishedAt: status === 'running' ? null : ago(60_000),
  pagesFetched: 12, entriesSeen: 1982, playersDiscovered: 1982, backfillsEnqueued: 1982,
  matchIdsSeen: 21137, matchesQueued: 0, pendingLegs: status === 'running' ? 30 : 0, apexCapped: [], ...extra,
});
// Older finished runs behind DONE, enough that the history pages: one failed.
const OLD = Array.from({ length: 11 }, (_, i) => crawl(`01K00000000000000000000LD${String(i).padStart(2, '0')}`, i === 3 ? 'failed' : 'completed', 'archive', {
  platform: i % 2 ? 'euw1' : 'oc1', startedAt: ago(86_400_000 * (i + 2)), finishedAt: ago(86_400_000 * (i + 2) - 3_600_000),
}));
const job = (kind, state, payload, extra = {}) => ({
  id: `J-${kind}-${state}-${Math.random()}`, kind, dedupeKey: null, priority: 20003, state, attempts: 1,
  runAfter: ago(1000), claimedAt: state === 'running' ? ago(30_000) : null, finishedAt: null, error: null, payload, ...extra,
});
const collect = (offset, state, extra) => job('ladder:collect', state, { crawlId: RUNNING, platform: 'oc1', queue: 'RANKED_SOLO_5x5', offset, puuids: Array(25).fill('p') }, extra);

const ACTIVITY = {
  [RUNNING]: {
    crawl: crawl(RUNNING, 'running', 'collect'),
    asOf: new Date().toISOString(),
    stages: [
      { name: 'enumerate', state: 'done', done: 3, total: 3, unit: 'legs', recent: 0, etaSeconds: null },
      { name: 'collect', state: 'now', done: 50, total: 80, unit: 'batches', recent: 10, etaSeconds: 1800 },
      { name: 'archive', state: 'waiting', done: 0, total: null, unit: 'ids', recent: 0, etaSeconds: null },
    ],
    openLegs: [{ leg: 'ladder:collect:1250', page: null }, { leg: 'ladder:collect:1275', page: null }],
    running: [collect(1250, 'running')],
    next: [collect(1275, 'pending'), collect(1300, 'pending', { runAfter: soon(120_000), attempts: 2 })],
    ahead: 4,
    failed: [collect(25, 'failed', { error: 'RIOT_UNAVAILABLE: match-v5 503 after 5 tries' })],
    downloads: { platform: 'oc1', ready: 120, delayed: 3, running: 2, failed: 1, recent: 60, etaSeconds: 1250 },
  },
  // A kr-sized run: Riot's Master list came back at its 10,000 cap (LAD-01).
  [DONE]: {
    crawl: crawl(DONE, 'completed', 'archive', { playersDiscovered: 11000, apexCapped: ['MASTER'] }),
    asOf: new Date().toISOString(),
    stages: [
      { name: 'enumerate', state: 'done', done: 3, total: 3, unit: 'legs', recent: 0, etaSeconds: null },
      { name: 'collect', state: 'done', done: 80, total: 80, unit: 'batches', recent: 0, etaSeconds: null },
      { name: 'archive', state: 'done', done: 21137, total: 21137, unit: 'ids', recent: 0, etaSeconds: null },
    ],
    openLegs: [], running: [], next: [], ahead: null, failed: [],
    downloads: { platform: 'oc1', ready: 0, delayed: 0, running: 0, failed: 0, recent: 0, etaSeconds: null },
  },
};
// Alike jobs (kind and platform) share a row in the panel, wherever they sit in the list.
const QUEUE = {
  running: [
    collect(1250, 'running'),
    job('archive:match', 'running', { matchId: 'OC1_700001' }, { priority: 100 }),
    collect(825, 'running', { claimedAt: ago(180_000) }),
  ],
  next: [
    job('archive:match', 'pending', { matchId: 'OC1_700002' }, { priority: 100 }),
    collect(1275, 'pending'),
    collect(1600, 'pending', { payload: { crawlId: RUNNING, platform: 'oc1', queue: 'RANKED_SOLO_5x5', offset: 1600, puuids: Array(17).fill('p') } }),
    collect(1375, 'pending', { attempts: 2 }),
    collect(25, 'pending', { payload: { crawlId: RUNNING, platform: 'kr', queue: 'RANKED_SOLO_5x5', offset: 25, puuids: Array(25).fill('p') } }),
    job('ranks:lookup', 'pending', { platform: 'oc1', queue: 'RANKED_SOLO_5x5', planned: true }, { priority: 20006, attempts: 0 }),
  ],
  ready: 5, delayed: 3, nextDelayedAt: soon(90_000),
};

// `GET /v1/admin/ladder/options`, trimmed to two platforms.
const OPTIONS = {
  platforms: [{ id: 'euw1', label: 'Europe West' }, { id: 'oc1', label: 'Oceania' }],
  queues: ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR'],
  tiers: ['IRON', 'BRONZE', 'SILVER', 'GOLD', 'PLATINUM', 'EMERALD', 'DIAMOND', 'MASTER', 'GRANDMASTER', 'CHALLENGER'],
  defaults: { platform: 'oc1', queue: 'RANKED_SOLO_5x5', tierFloor: 'MASTER', backfillLimit: 100 },
};

// Flip `scene.running` off for a page with no crawl in flight.
const scene = { running: true };

// `opts.body` is the request body as sent; a POST's is recorded on `calls.bodies`.
function api(calls, url, opts) {
  const u = new URL(url, 'http://localhost');
  calls.push(`${u.pathname}${u.search}`);
  const p = u.pathname;
  if (p === '/dashboard/config.json') return [200, { authDisabled: true }];
  if (p === '/v1/admin/ladder/crawls') return [200, { crawls: [...(scene.running ? [crawl(RUNNING, 'running', 'collect')] : []), crawl(DONE, 'completed', 'archive'), ...OLD] }];
  const m = p.match(/^\/v1\/admin\/ladder\/crawls\/(\w+)$/);
  if (m && opts.method === 'DELETE') return [200, { ok: true, crawlId: m[1], status: 'cancelled', droppedJobs: 3 }];
  if (m) return ACTIVITY[m[1]] ? [200, ACTIVITY[m[1]]] : [404, { error: { code: 'NOT_FOUND', message: 'No such ladder crawl' } }];
  if (p === '/v1/admin/jobs/queue') return [200, QUEUE];
  if (p === '/v1/admin/ladder/options') return [200, OPTIONS];
  if (p === '/v1/admin/analytics/recompute' && opts.method === 'POST') {
    const body = JSON.parse(opts.body ?? '{}');
    (calls.bodies ??= []).push([p, body]);
    if (body.platform === 'euw1') return [400, { error: { code: 'VALIDATION', message: 'not in this test: euw1' } }];
    return [202, { ok: true, platform: body.platform, queue: body.queue }];
  }
  return [503, { error: { code: 'UNAVAILABLE', message: 'not in this test' } }];
}

export { html, RUNNING, DONE, ACTIVITY, QUEUE, OPTIONS, api, scene };

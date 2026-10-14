// The fake admin API the dashboard suites drive src/ui/admin.html against
// (admin-*.test.mjs in jsdom, admin.browser.test.mjs in Chromium). Shapes are
// the OpenAPI document's: GET /v1/admin/info, /v1/admin/metrics,
// /v1/admin/backups and /readyz. `scene` changes what it answers.
import { readFileSync } from 'node:fs';

export const html = readFileSync(new URL('../../src/ui/admin.html', import.meta.url), 'utf8');

export const KEY = 'rpx_test_admin_key_0001';
const NOW = Date.now();

export const INFO = {
  version: '2.3.0', gitSha: '41567bd0c0ff', rustc: 'rustc 1.98.1', builtAt: NOW - 86_400_000,
  startedAt: NOW - (3 * 86400 + 4 * 3600) * 1000, uptimeSeconds: 3 * 86400 + 4 * 3600,
  env: 'production', role: 'all', keyScope: '5f3a9c1e77aa', workers: 8,
  features: { devUi: false, docsUi: true, dashboardUi: true, authDisabled: false, archiveTimelines: true, staleWhileRevalidate: true, trustProxy: false, postgres: false },
  config: { PORT: '8080', BOOTSTRAP_ADMIN_KEY: '[REDACTED]' },
};

export function snapshot(over = {}) {
  const q = (o) => ({ active: 0, waiting: 0, prioritized: 0, delayed: 0, scheduled: 0, failed: 0, completed: 0, ...o });
  return {
    v: 1, keyScope: '5f3a9c1e77aa',
    totals: { archivedMatches: 230_715, trackedPlayers: 3, knownPlayers: 41_200, activeConsumers: 2 },
    queues: { archive: q({ active: 3, waiting: 1_126_303, failed: 12 }), backfill: q(), ddragon: q(), ladder: q({ waiting: 14, delayed: 2 }), maintenance: q({ delayed: 1 }), poll: q() },
    ws: { connections: 1, subscriptions: 2 }, events: {}, cache: { hit: 940, miss: 50, neg: 5, stale: 5 },
    limiter: [{ scope: 'oc1', kind: 'platform', label: 'Oceania', frozenMs: 0, windows: [], methods: [], interactiveWaiters: 0 }],
    worker: { alive: true, lastSeenMs: 0 }, flows: { backfillsQueued: {}, refreshClaims: {} },
    ladder: { running: [], lastCompleted: null, entries: 0 },
    analytics: { lastRuns: [], topChampions: [] },
    process: { uptimeSeconds: 3 * 86400 + 4 * 3600, rssBytes: 412 * 1024 * 1024 },
    sampledAt: { fast: NOW, slow: NOW },
    cacheStore: { l1Entries: 10, l1Bytes: 2048, l1MaxBytes: 128 * 1024 * 1024, l2Rows: 4, l2Dropped: 0 },
    gates: { bulkUsageCeiling: 0.8, bulkIdleCeiling: 0.95, bulkIdleAfterS: 30, clientWaitBudgetMs: 2000, blockedScopes: [], blockedMethods: [] },
    inflight: 0, quotas: [], sqlite: { readers: 6, readersFree: 6 },
    disk: { dbBytes: 22 * 1024 ** 3, walBytes: 0, freelistBytes: 0, freeBytes: 51 * 1024 ** 3, totalBytes: 115 * 1024 ** 3 },
    requests: { '/v1/lol/x|200|hit': 100, '/v1/lol/x|502|none': 2 },
    upstream: { 'sea|match.byId|200': 400 },
    ...over,
  };
}

/** 1440 points at 60 s; the oldest 100 predate ADM-04 and lack disk, requests and upstream. */
export function history(n = 1440) {
  const points = [];
  for (let i = 0; i < n; i++) {
    const t = NOW - (n - i) * 60_000;
    const old = i < 100;
    points.push({
      t,
      totals: { archivedMatches: 200_000 + i * 20, trackedPlayers: 3, knownPlayers: 41_000 },
      queues: { active: 3, pending: 1_200_000 - i * 50, failed: 12 },
      analytics: { rows: 0, ageSeconds: null },
      cache: { hit: i * 90, miss: i * 10, neg: 0, stale: 0 },
      ...(old ? {} : { disk: { dbBytes: 22 * 1024 ** 3, walBytes: 0, freelistBytes: 0, freeBytes: (60 - i / 160) * 1024 ** 3, totalBytes: 115 * 1024 ** 3 }, requests: i * 30, upstream: i * 48 }),
    });
  }
  return { intervalS: 60, maxPoints: 1440, points };
}

const ROUTES = ['/v1/lol/summoners/by-puuid/:platform/:puuid', '/v1/riot/account/v1/accounts/by-riot-id/:region/:gameName/:tagLine', '/v1/admin/stats'];
/** The recent-requests ring, newest first. */
export function recent(n = 60) {
  return Array.from({ length: n }, (_, i) => ({
    at: NOW - i * 1000,
    method: 'GET',
    route: ROUTES[i % 3],
    path: ROUTES[i % 3].replace(':platform', 'oc1').replace(':puuid', `p${i}`).replace(':region', 'asia').replace(':gameName', 'Hide on bush').replace(':tagLine', 'KR1'),
    status: i % 20 === 0 ? 502 : i % 7 === 0 ? 404 : 200,
    cache: i % 3 === 2 ? 'none' : i % 2 ? 'hit' : 'miss',
    latencyMs: 10 + i,
    consumer: i % 3 === 2 ? 'ops' : 'web',
    requestId: `01J00000000000000000REQ${String(i).padStart(3, '0')}`,
  }));
}

/** Three scopes: kr frozen, europe holding bulk work with a full method, oc1 quiet with many methods. */
export function limiterSnapshot() {
  const w = (window, used, limit) => ({ window, used, limit });
  const m = (method, used, limit) => ({ method, windows: [w(`${limit}:10`, used, limit)] });
  return snapshot({
    limiter: [
      { scope: 'oc1', kind: 'platform', label: 'Oceania', frozenMs: 0, interactiveWaiters: 0, windows: [w('20:1', 2, 20), w('100:120', 30, 100)],
        methods: [m('summoner.byPuuid', 10, 1600), m('league.entries', 5, 50), m('match.ids', 1, 2000), m('status', 0, 20), m('spectator', 3, 20000), m('mastery', 1, 20000)] },
      { scope: 'kr', kind: 'platform', label: 'Korea', frozenMs: 30_000, interactiveWaiters: 1, windows: [w('20:1', 20, 20), w('100:120', 99, 100)], methods: [] },
      { scope: 'europe', kind: 'region', label: 'Europe', frozenMs: 0, interactiveWaiters: 2, windows: [w('20:1', 9, 20), w('100:120', 85, 100)], methods: [m('match.byId', 1990, 2000)] },
    ],
    gates: { bulkUsageCeiling: 0.8, bulkIdleCeiling: 0.95, bulkIdleAfterS: 30, clientWaitBudgetMs: 2000,
      blockedScopes: [{ scope: 'europe', forMs: 12_000 }, { scope: 'kr', forMs: 30_000 }],
      blockedMethods: [{ scope: 'europe', method: 'match.byId', forMs: 4_000 }] },
    cacheStore: { l1Entries: 12_400, l1Bytes: 100 * 1024 * 1024, l1MaxBytes: 128 * 1024 * 1024, l2Rows: 3_210, l2Dropped: 7 },
  });
}

// ── jobs: the /dev/jobs fake's rows and traces (DEV-19), plus the bulk purge ──
export const RUN = '01K00000000000000000000RUN';
export const DONE = '01K0000000000000000000DONE';
export const FAILED = '01K00000000000000000000BAD';
export const PENDING = '01K0000000000000000000PEND';
const ago = (ms) => new Date(Date.now() - ms).toISOString();
const soon = (ms) => new Date(Date.now() + ms).toISOString();
const row = (id, kind, state, payload, extra = {}) => ({
  id, kind, dedupeKey: null, priority: 20_000, state, attempts: 1, runAfter: ago(60_000),
  claimedAt: state === 'running' ? ago(30_000) : null, finishedAt: null, error: null, payload, ...extra,
});
export function jobsWorld() {
  const walk = { crawlId: 'C1', platform: 'oc1', queue: 'RANKED_SOLO_5x5', tier: 'GOLD', division: 'II' };
  const collect = (i, platform, state) => row(`01K000000000000000000COL${String(i).padStart(2, '0')}`, 'ladder:collect', state, { crawlId: 'C1', platform, queue: 'RANKED_SOLO_5x5', offset: i * 25, puuids: Array(25).fill('p') }, { priority: 20_003, runAfter: state === 'pending' && i === 7 ? soon(65_000) : ago(1000), attempts: i === 3 ? 2 : 1 });
  const jobs = {
    [RUN]: row(RUN, 'ladder:walk', 'running', walk),
    [DONE]: row(DONE, 'archive:match', 'done', { matchId: 'OC1_700001' }, { finishedAt: ago(5_000) }),
    [FAILED]: row(FAILED, 'archive:match', 'failed', { matchId: 'OC1_700002' }, { attempts: 5, finishedAt: ago(9_000), error: 'RIOT_UNAVAILABLE: match-v5 503' }),
    [PENDING]: row(PENDING, 'backfill:player', 'pending', { puuid: 'p'.repeat(78), platform: 'oc1', limit: 100 }, { runAfter: soon(30_000) }),
  };
  // Up next, out of offset order and over two platforms (ADR-093's grouping).
  const next = [collect(4, 'kr', 'pending'), collect(1, 'kr', 'pending'), collect(9, 'euw1', 'pending'), collect(3, 'kr', 'pending'), collect(7, 'euw1', 'pending'), jobs[PENDING]];
  const runEvents = [
    { seq: 0, at: ago(30_000), kind: 'job', text: 'claimed by worker 1 (attempt 1)', ms: null },
    { seq: 1, at: ago(29_000), kind: 'step', text: 'GOLD II page 1 on oc1', ms: null },
    { seq: 2, at: ago(28_500), kind: 'riot', text: 'league.entriesByTier /lol/league/v4/entries/RANKED_SOLO_5x5/GOLD/II → MISS', ms: 410 },
  ];
  const traces = {
    [RUN]: { kind: 'ladder:walk', worker: 1, attempt: 1, startedAt: ago(30_000), finishedAt: null, outcome: null, now: 'GOLD II page 1 on oc1', nowSince: ago(29_000), dropped: 0 },
    [DONE]: { kind: 'archive:match', worker: 2, attempt: 1, startedAt: ago(6_000), finishedAt: ago(5_000), outcome: 'done', now: null, nowSince: null, dropped: 0 },
  };
  const doneEvents = [
    { seq: 0, at: ago(6_000), kind: 'job', text: 'claimed by worker 2 (attempt 1)', ms: null },
    { seq: 1, at: ago(5_500), kind: 'wait', text: 'waited for the americas rate limit (match.byId)', ms: 300 },
    { seq: 2, at: ago(5_000), kind: 'job', text: 'done', ms: 1000 },
  ];
  // Bulk-purgeable rows: archive:match pending at two priorities.
  const bulk = { 'archive:match|pending': [{ priority: 20_005, n: 1_126_303 }, { priority: 100, n: 7 }], 'archive:match|failed': [{ priority: 20_005, n: 1 }] };
  function grow() {
    const page = runEvents.filter((e) => e.kind === 'step').length + 1;
    const text = `GOLD II page ${page} on oc1`;
    runEvents.push({ seq: runEvents.length, at: new Date().toISOString(), kind: 'step', text, ms: null });
    Object.assign(traces[RUN], { now: text, nowSince: new Date().toISOString() });
  }
  function handle(url, method) {
    const u = new URL(url, 'http://x');
    const p = u.pathname;
    if (p === '/v1/admin/jobs/activity') {
      return [200, {
        workers: [
          { worker: 1, jobId: RUN, kind: 'ladder:walk', since: ago(30_000), now: traces[RUN].now, nowSince: traces[RUN].nowSince, events: runEvents.length },
          { worker: 2, jobId: null, kind: null, since: ago(5_000), now: null, nowSince: null, events: 0 },
        ],
        finished: [{ jobId: DONE, kind: 'archive:match', worker: 2, startedAt: ago(6_000), finishedAt: ago(5_000), outcome: 'done' }],
      }];
    }
    if (p === '/v1/admin/jobs/queue') return [200, { running: [jobs[RUN]], next, ready: 5, delayed: 1, nextDelayedAt: jobs[PENDING].runAfter, heldKinds: ['aggregate:analytics'], held: 2 }];
    if (p === '/v1/admin/jobs/stats') {
      return [200, {
        kinds: {
          'ladder:walk': { pending: 0, running: 1, done: 3, failed: 0 },
          'archive:match': { pending: 1_126_310, running: 0, done: 50, failed: Object.values(jobs).filter((j) => j.kind === 'archive:match' && j.state === 'failed').length },
          'names:backfill': { pending: 0, running: 0, done: 1, failed: 0 },
        },
        totals: { pending: 1_126_310, running: 1, done: 54, failed: 1 },
      }];
    }
    if (p === '/v1/admin/jobs' && method === 'DELETE') {
      const q = u.searchParams;
      const kind = q.get('kind');
      const st = q.get('state');
      const min = Number(q.get('minPriority') ?? 0);
      const rows = bulk[`${kind}|${st}`] ?? [];
      const matched = rows.filter((r) => r.priority >= min).reduce((n, r) => n + r.n, 0);
      const dry = q.get('dryRun') === 'true';
      if (!dry) bulk[`${kind}|${st}`] = rows.filter((r) => r.priority < min);
      return [200, { kind, state: st, minPriority: min, matched, deleted: dry ? 0 : matched, dryRun: dry }];
    }
    if (p === '/v1/admin/jobs') return [200, { jobs: Object.values(jobs).filter((j) => j.state === (u.searchParams.get('state') ?? j.state)) }];
    let m = p.match(/^\/v1\/admin\/jobs\/(\w+)\/activity$/);
    if (m) {
      const id = m[1];
      if (!jobs[id]) return [404, { error: { code: 'NOT_FOUND', message: 'No such job' } }];
      const after = Number(u.searchParams.get('after') ?? 0);
      if (id === RUN && after > 0) grow();
      const events = id === RUN ? runEvents : id === DONE ? doneEvents : [];
      const t = traces[id];
      return [200, { job: jobs[id], trace: t ? { ...t, events: events.filter((e) => e.seq >= after), nextSeq: events.length } : null }];
    }
    m = p.match(/^\/v1\/admin\/jobs\/(\w+)\/retry$/);
    if (m && method === 'POST') {
      Object.assign(jobs[m[1]], { state: 'pending', attempts: 0, error: null, finishedAt: null, runAfter: new Date().toISOString() });
      return [200, jobs[m[1]]];
    }
    m = p.match(/^\/v1\/admin\/jobs\/(\w+)$/);
    if (m && method === 'DELETE') {
      delete jobs[m[1]];
      return [200, { ok: true }];
    }
    return null;
  }
  return { handle, jobs, bulk };
}

// ── ladder: dashboard.test.mjs's crawls, activity and options (DEV-13, DEV-17) ──
export const CRAWL = '01K0000000000000000000RUN1';
export const PAST = '01K0000000000000000000DON1';
const crawl = (id, status, phase, extra = {}) => ({
  id, platform: 'oc1', queue: 'RANKED_SOLO_5x5', tierFloor: 'MASTER', status, phase,
  startedAt: ago(3_600_000), finishedAt: status === 'running' ? null : ago(60_000),
  pagesFetched: 12, entriesSeen: 1982, playersDiscovered: 1982, backfillsEnqueued: 1982,
  matchIdsSeen: 21137, matchesQueued: 0, pendingLegs: status === 'running' ? 30 : 0, apexCapped: [], ...extra,
});
const OLD = Array.from({ length: 11 }, (_, i) => crawl(`01K00000000000000000000LD${String(i).padStart(2, '0')}`, i === 3 ? 'failed' : 'completed', 'archive', {
  platform: i % 2 ? 'euw1' : 'oc1', startedAt: ago(86_400_000 * (i + 2)), finishedAt: ago(86_400_000 * (i + 2) - 3_600_000),
}));
const ljob = (kind, state, payload, extra = {}) => ({
  id: `J-${kind}-${state}-${Math.random()}`, kind, dedupeKey: null, priority: 20003, state, attempts: 1,
  runAfter: ago(1000), claimedAt: state === 'running' ? ago(30_000) : null, finishedAt: null, error: null, payload, ...extra,
});
const lcollect = (offset, state, extra) => ljob('ladder:collect', state, { crawlId: CRAWL, platform: 'oc1', queue: 'RANKED_SOLO_5x5', offset, puuids: Array(25).fill('p') }, extra);
export function ladderWorld() {
  const activity = {
    [CRAWL]: {
      crawl: crawl(CRAWL, 'running', 'collect'), asOf: new Date().toISOString(),
      stages: [
        { name: 'enumerate', state: 'done', done: 3, total: 3, unit: 'legs', recent: 0, etaSeconds: null },
        { name: 'collect', state: 'now', done: 50, total: 80, unit: 'batches', recent: 10, etaSeconds: 1800 },
        { name: 'archive', state: 'waiting', done: 0, total: null, unit: 'ids', recent: 0, etaSeconds: null },
      ],
      openLegs: [{ leg: 'ladder:collect:1250', page: null }, { leg: 'ladder:collect:1275', page: null }],
      running: [lcollect(1250, 'running')],
      next: [lcollect(1275, 'pending'), lcollect(1300, 'pending', { runAfter: soon(120_000), attempts: 2 })],
      ahead: 4,
      failed: [lcollect(25, 'failed', { error: 'RIOT_UNAVAILABLE: match-v5 503 after 5 tries' })],
      downloads: { platform: 'oc1', ready: 120, delayed: 3, running: 2, failed: 1, recent: 60, etaSeconds: 1250 },
    },
    // A kr-sized run: Riot's Master list came back at its 10,000 cap (LAD-01).
    [PAST]: {
      crawl: crawl(PAST, 'completed', 'archive', { playersDiscovered: 11000, apexCapped: ['MASTER'] }), asOf: new Date().toISOString(),
      stages: [
        { name: 'enumerate', state: 'done', done: 3, total: 3, unit: 'legs', recent: 0, etaSeconds: null },
        { name: 'collect', state: 'done', done: 80, total: 80, unit: 'batches', recent: 0, etaSeconds: null },
        { name: 'archive', state: 'done', done: 21137, total: 21137, unit: 'ids', recent: 0, etaSeconds: null },
      ],
      openLegs: [], running: [], next: [], ahead: null, failed: [],
      downloads: { platform: 'oc1', ready: 0, delayed: 0, running: 0, failed: 0, recent: 0, etaSeconds: null },
    },
  };
  const options = {
    platforms: [{ id: 'euw1', label: 'Europe West' }, { id: 'oc1', label: 'Oceania' }],
    queues: ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR'],
    tiers: ['IRON', 'BRONZE', 'SILVER', 'GOLD', 'PLATINUM', 'EMERALD', 'DIAMOND', 'MASTER', 'GRANDMASTER', 'CHALLENGER'],
    defaults: { platform: 'oc1', queue: 'RANKED_SOLO_5x5', tierFloor: 'MASTER', backfillLimit: 100 },
  };
  const w = { running: true, bodies: [] };
  w.handle = (url, method, body) => {
    const u = new URL(url, 'http://x');
    const p = u.pathname;
    if (p === '/v1/admin/ladder/crawls') return [200, { crawls: [...(w.running ? [crawl(CRAWL, 'running', 'collect')] : []), crawl(PAST, 'completed', 'archive'), ...OLD] }];
    const m = p.match(/^\/v1\/admin\/ladder\/crawls\/(\w+)$/);
    if (m && method === 'DELETE') { w.running = false; return [200, { ok: true, crawlId: m[1], status: 'cancelled', droppedJobs: 3 }]; }
    if (m) return activity[m[1]] ? [200, activity[m[1]]] : [404, { error: { code: 'NOT_FOUND', message: 'No such ladder crawl' } }];
    if (p === '/v1/admin/ladder/options') return [200, options];
    if (p === '/v1/admin/ladder/crawl' && method === 'POST') {
      const b = JSON.parse(body ?? '{}');
      w.bodies.push([p, b]);
      return [202, { crawlId: '01K0000000000000000000NEW1', status: 'started', platform: b.platform, queue: b.queue, legs: 7 }];
    }
    if (p === '/v1/admin/analytics/recompute' && method === 'POST') {
      const b = JSON.parse(body ?? '{}');
      w.bodies.push([p, b]);
      if (b.platform === 'euw1') return [400, { error: { code: 'VALIDATION', message: 'not in this test: euw1' } }];
      return [202, { ok: true, platform: b.platform, queue: b.queue }];
    }
    return null;
  };
  return w;
}
/** A snapshot with the running crawl in its ladder section, and an analytics run. */
export function ladderSnapshot(running = true) {
  return snapshot({
    ladder: { running: running ? [crawl(CRAWL, 'running', 'collect')] : [], lastCompleted: crawl(PAST, 'completed', 'archive'), entries: 48_210 },
    analytics: {
      lastRuns: [{ platform: 'oc1', queue: 'RANKED_SOLO_5x5', at: Date.now() - 600_000, status: 'completed', ms: 1234, steps: { champions: 0.5, matchups: 1.25 }, rows: { champion_stats: 3, champion_items: 7 }, games: 30 }],
      topChampions: [{ championId: 103, championName: 'Ahri', games: 6, winRate: 0.5 }],
    },
  });
}

// ── consumers and tracked players ──
export const ADMIN_ID = '01K000000000000000000ADMIN';
export function consumersWorld() {
  const w = {
    revokedSelf: false,
    consumers: [
      { id: ADMIN_ID, name: 'ops', scopes: ['read', 'admin'], quotaPerMin: 10000, createdAt: ago(86_400_000 * 30), disabledAt: null },
      { id: '01K0000000000000000000WEB1', name: 'ninjagoldfinch.lol', scopes: ['read'], quotaPerMin: 600, createdAt: ago(86_400_000 * 7), disabledAt: null },
      { id: '01K0000000000000000000OLD1', name: 'old-site', scopes: ['read'], quotaPerMin: 600, createdAt: ago(86_400_000 * 90), disabledAt: ago(86_400_000 * 60) },
    ],
    players: [
      { puuid: 'P'.repeat(78), keyScope: '5f3a', platform: 'kr', gameName: 'Hide on bush', tagLine: 'KR1', tracked: true, lastSeenMatchId: 'KR_1', historyBackfillStartedAt: ago(3600_000), historyBackfilledAt: ago(3500_000), historyBackfillDepth: 100, updatedAt: ago(60_000) },
    ],
    bodies: [],
  };
  w.handle = (url, method, body) => {
    const u = new URL(url, 'http://x');
    const p = u.pathname;
    const b = body ? JSON.parse(body) : null;
    if (p === '/v1/admin/consumers' && method === 'GET') return [200, { consumers: w.consumers }];
    if (p === '/v1/admin/consumers' && method === 'POST') {
      w.bodies.push([p, b]);
      const c = { id: '01K0000000000000000000NEW1', name: b.name, scopes: b.scopes, quotaPerMin: b.quotaPerMin, createdAt: new Date().toISOString(), disabledAt: null };
      w.consumers.push(c);
      return [200, { id: c.id, name: c.name, scopes: c.scopes, quotaPerMin: c.quotaPerMin, key: 'rpx_NEWKEY_shown_once_0123456789', warning: 'Store this key now. It cannot be retrieved again.' }];
    }
    let m = p.match(/^\/v1\/admin\/consumers\/(\w+)\/revoke-cache$/);
    if (m) { w.bodies.push([p, b]); return [200, { ok: true, id: m[1], stillActive: true }]; }
    m = p.match(/^\/v1\/admin\/consumers\/(\w+)$/);
    if (m && method === 'DELETE') {
      const c = w.consumers.find((x) => x.id === m[1]);
      c.disabledAt = new Date().toISOString();
      if (c.id === ADMIN_ID) w.revokedSelf = true;
      return [200, { ok: true, id: c.id }];
    }
    if (p === '/v1/admin/tracked-players' && method === 'GET') return [200, { players: u.searchParams.get('tracked') === 'true' ? w.players.filter((x) => x.tracked) : w.players }];
    if (p === '/v1/admin/tracked-players' && method === 'POST') {
      w.bodies.push([p, b]);
      const pl = { puuid: b.puuid ?? 'N'.repeat(78), keyScope: '5f3a', platform: b.platform, gameName: b.gameName ?? null, tagLine: b.tagLine ?? null, tracked: true, lastSeenMatchId: null, historyBackfillStartedAt: null, historyBackfilledAt: null, historyBackfillDepth: null, updatedAt: new Date().toISOString() };
      w.players.push(pl);
      return [200, { ...pl, backfill: null }];
    }
    m = p.match(/^\/v1\/admin\/tracked-players\/(\w+)$/);
    if (m && method === 'DELETE') { w.players.find((x) => x.puuid === m[1]).tracked = false; return [200, { ok: true, puuid: m[1], tracked: false }]; }
    return null;
  };
  return w;
}

export const scene = {
  // What GET /v1/admin/info answers without a key: 401, or 200 when AUTH_DISABLED.
  authDisabled: false,
  snapshot: snapshot(),
  backups: { dir: '/data/backups', keep: 14, files: [{ name: 'riot-proxy-2026-10-13.db', bytes: 21 * 1024 ** 3, modifiedAt: NOW - 7 * 3_600_000 }], lastRun: null, diskFreeBytes: 51 * 1024 ** 3 },
  ready: { ok: true, sqlite: true, limiter: true, readers: 6, readersFree: 6 },
  history: history(),
  stats: {
    keyScope: '5f3a9c1e77aa', archivedMatches: 230_715, trackedPlayers: 3, knownPlayers: 41_200, archivedTimelines: 216_475,
    archiveStoredBytes: 2_470_000_000, archiveRawBytes: 16_000_000_000,
    archiveBodies: { matches: { local: 715, packed: 30_000, offloaded: 0 }, timelines: { local: 475, packed: 16_000, offloaded: 0 } },
    archivePacks: 0, archivePackBytes: 0, archivePackLiveBytes: 0, dbFileBytes: 22 * 1024 ** 3, walBytes: 64 * 1024 ** 2,
    dbPageSize: 4096, dbPageCount: 5_767_168, dbFreelistPages: 262_144, diskFreeBytes: 51 * 1024 ** 3, diskTotalBytes: 115 * 1024 ** 3,
    sampledAt: { fast: NOW, slow: NOW - 30_000 },
  },
  timelines: {
    configuredPatches: 2,
    patches: [{ patch: '16.20', matches: 900, withTimeline: 600, notFound: 20, left: 280 }],
    regions: [{ region: 'sea', left: 200, patch: '16.20' }, { region: 'asia', left: 80, patch: '16.20' }],
    totals: { patch: null, matches: 900, withTimeline: 600, notFound: 20, left: 280 },
  },
  posted: [],
  consumers: consumersWorld(),
  jobs: jobsWorld(),
  ladder: ladderWorld(),
  // Cache keys the purge matches against.
  cacheKeys: ['5f3a:summoner.byPuuid:kr:a', '5f3a:summoner.byPuuid:kr:b', '5f3a:league.entries:kr:c'],
  recent: recent(),
  errors: [
    { at: NOW - 60_000, code: 'UPSTREAM_ERROR', status: 502, message: 'Upstream request failed', requestId: '01J00000000000000000REQ000' },
    { at: NOW - 120_000, code: 'NOT_FOUND', status: 404, message: 'No route for GET /nowhere', requestId: '01J00000000000000000REQ007' },
  ],
};

const unauthorized = [401, { error: { code: 'UNAUTHORIZED', message: 'Missing or invalid API key' } }];
// The server's own messages (src/http/auth.rs).
const forbidden = [403, { error: { code: 'FORBIDDEN', message: "This key lacks the 'admin' scope" } }];
const farAway = [403, { error: { code: 'FORBIDDEN', message: 'Admin access is not permitted from this address' } }];

/** `[status, body]` for one call; `calls` records `{ url, method, auth }`. */
export function api(calls, url, { method = 'GET', auth = null, body = null } = {}) {
  calls.push({ url, method, auth, body });
  const path = new URL(url, 'http://x').pathname;
  if (path === '/readyz') return [200, scene.ready];
  if (path.startsWith('/v1/admin/')) {
    if (scene.consumers.revokedSelf) return unauthorized;
    if (!auth && !scene.authDisabled) return unauthorized;
    if (auth && auth !== `Bearer ${KEY}`) return { 'Bearer rpx_reader': forbidden, 'Bearer rpx_far': farAway }[auth] ?? unauthorized;
  }
  if (path.startsWith('/v1/admin/ladder') || path === '/v1/admin/analytics/recompute') {
    const r = scene.ladder.handle(url, method, body);
    if (r) return r;
  }
  if (path.startsWith('/v1/admin/consumers') || path.startsWith('/v1/admin/tracked-players')) {
    const r = scene.consumers.handle(url, method, body);
    if (r) return r;
  }
  if (path.startsWith('/v1/admin/jobs')) {
    const r = scene.jobs.handle(url, method);
    if (r) return r;
  }
  switch (path) {
    case '/v1/admin/info': return [200, INFO];
    case '/v1/admin/metrics': return [200, scene.snapshot];
    case '/v1/admin/backups': return [200, scene.backups];
    case '/v1/admin/stats': return [200, scene.stats];
    case '/v1/admin/timelines/backfill': return [200, scene.timelines];
    case '/v1/admin/maintenance/run': scene.posted.push(path); return [202, { ok: true, jobId: '01KMAINT', status: scene.posted.filter((x) => x === path).length > 1 ? 'already-queued' : 'queued' }];
    case '/v1/admin/analytics/reextract': scene.posted.push(path); return [202, { ok: true, stale: 1234 }];
    case '/v1/admin/ddragon/sync': scene.posted.push(path); return [200, { ok: true, jobId: '01KDD' }];
    case '/v1/admin/players/names/backfill': scene.posted.push(path); return [202, { ok: true, unnamed: 17 }];
    case '/v1/admin/metrics/history': return [200, scene.history];
    case '/v1/admin/debug/cache': {
      const q = new URL(url, 'http://x').searchParams;
      if (!q.get('method')) return [400, { error: { code: 'VALIDATION', message: "querystring must have required property 'method'" } }];
      const present = q.get('method') === 'summoner.byPuuid';
      return [200, { key: `5f3a:${q.get('method')}:${q.get('scope')}:x`, present, ageSeconds: present ? 125 : null, stale: present ? false : null }];
    }
    case '/v1/admin/cache/purge': {
      const b = JSON.parse(body ?? '{}');
      const re = new RegExp(`^5f3a:${String(b.pattern).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')}$`);
      const hits = scene.cacheKeys.filter((k) => re.test(k));
      if (b.dryRun) return [200, { ok: true, pattern: b.pattern, matched: hits.length, dryRun: true }];
      scene.cacheKeys = scene.cacheKeys.filter((k) => !hits.includes(k));
      return [200, { ok: true, pattern: b.pattern, deleted: hits.length }];
    }
    case '/v1/admin/recent': {
      const q = new URL(url, 'http://x').searchParams;
      const limit = Number(q.get('limit') ?? 100);
      return q.get('kind') === 'errors'
        ? [200, { kind: 'errors', capacity: 200, errors: scene.errors.slice(0, limit) }]
        : [200, { kind: 'requests', capacity: 500, requests: scene.recent.slice(0, limit) }];
    }
    default:
      if (path.startsWith('/v1/admin/limits/')) {
        const scope = decodeURIComponent(path.split('/').pop());
        return [200, { scope, usage: [{ window: '20:1', used: 4, limit: 20 }, { window: '100:120', used: 40, limit: 100 }], frozenMs: scope === 'kr' ? 25_000 : 0 }];
      }
      return [404, { error: { code: 'NOT_FOUND', message: `No route for ${method} ${path}` } }];
  }
}

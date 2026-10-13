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

export const scene = {
  // What GET /v1/admin/info answers without a key: 401, or 200 when AUTH_DISABLED.
  authDisabled: false,
  snapshot: snapshot(),
  backups: { dir: '/data/backups', keep: 14, files: [{ name: 'riot-proxy-2026-10-13.db', bytes: 21 * 1024 ** 3, modifiedAt: NOW - 7 * 3_600_000 }], lastRun: null, diskFreeBytes: 51 * 1024 ** 3 },
  ready: { ok: true, sqlite: true, limiter: true, readers: 6, readersFree: 6 },
};

const unauthorized = [401, { error: { code: 'UNAUTHORIZED', message: 'Missing or invalid API key' } }];
const forbidden = [403, { error: { code: 'FORBIDDEN', message: 'This key lacks the admin scope' } }];

/** `[status, body]` for one call; `calls` records `{ url, method, auth }`. */
export function api(calls, url, { method = 'GET', auth = null } = {}) {
  calls.push({ url, method, auth });
  const path = new URL(url, 'http://x').pathname;
  if (path === '/readyz') return [200, scene.ready];
  if (path.startsWith('/v1/admin/')) {
    if (!auth && !scene.authDisabled) return unauthorized;
    if (auth && auth !== `Bearer ${KEY}`) return auth === 'Bearer rpx_reader' ? forbidden : unauthorized;
  }
  switch (path) {
    case '/v1/admin/info': return [200, INFO];
    case '/v1/admin/metrics': return [200, scene.snapshot];
    case '/v1/admin/backups': return [200, scene.backups];
    default: return [404, { error: { code: 'NOT_FOUND', message: `No route for ${method} ${path}` } }];
  }
}

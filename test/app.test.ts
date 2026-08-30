import './helpers/env.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { buildApp, type App } from '../src/app.js';
import { closeDb, pingDb } from '../src/db/index.js';
import {
  createTestConsumer,
  removeTestConsumers,
  testConsumerName,
  trackTestConsumer,
} from './helpers/consumers.js';
import { ipAllowed } from '../src/auth/plugin.js';
import { closeRedis, redis } from '../src/redis.js';
import { wsHub } from '../src/ws/index.js';

/**
 * HTTP-surface tests: auth, validation and the error envelope (§6.1, §12).
 * Requires Redis and Postgres; skipped when they are not reachable so the
 * unit suite still runs standalone.
 */
let app: App | undefined;
let readKey = '';
let adminKey = '';
let available = false;

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const read = await createTestConsumer({ name: testConsumerName('read'), scopes: ['read'] });
  const admin = await createTestConsumer({
    name: testConsumerName('admin'),
    scopes: ['read', 'admin'],
  });
  readKey = read?.key ?? '';
  adminKey = admin?.key ?? '';

  app = await buildApp();
  await app.ready();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  // Before the handles close: the delete needs the pool.
  if (available) await removeTestConsumers();
  await Promise.allSettled([closeRedis(), closeDb()]);
});

const auth = (key: string) => ({ authorization: `Bearer ${key}` });

describe('http surface', () => {
  it('serves /healthz without a key (FR-12)', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/healthz' });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ ok: true });
  });

  it('serves Prometheus metrics without a key', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/metrics' });
    expect(res.statusCode).toBe(200);
    expect(res.body).toContain('proxy_requests_total');
  });

  it('401s an unauthenticated request in the error envelope (§6.1)', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/lol/status/euw1',
    });
    expect(res.statusCode).toBe(401);
    expect(res.json()).toEqual({
      error: { code: 'UNAUTHORIZED', message: expect.any(String) },
    });
  });

  it('401s a syntactically valid but unknown key', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/lol/status/euw1',
      headers: auth('rpx_thiskeydoesnotexistatall000000'),
    });
    expect(res.statusCode).toBe(401);
    expect(res.json().error.code).toBe('UNAUTHORIZED');
  });

  it('400s an unknown platform with BAD_REGION (§6.1)', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/lol/status/euw',
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(400);
    expect(res.json().error.code).toBe('BAD_REGION');
  });

  it('400s an unknown region on a regional route', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/riot/accounts/by-riot-id/eu/Faker/KR1',
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(400);
    expect(res.json().error.code).toBe('BAD_REGION');
  });

  it("rejects a match id count above Riot's maximum of 100 (§12.3)", async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: `/v1/lol/matches/ids/europe/${'P'.repeat(78)}?count=500`,
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(400);
    expect(res.json().error.code).toBe('VALIDATION');
  });

  it("rejects a Riot ID past Riot's documented maxima (§12.3)", async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/riot/accounts/by-riot-id/europe/Faker/TOOLONG',
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(400);
    expect(res.json().error.code).toBe('VALIDATION');
  });

  it('403s a read key on an admin route (§12.1)', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/stats',
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(403);
    expect(res.json().error.code).toBe('FORBIDDEN');
  });

  it('allows an admin key on an admin route', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/stats',
      headers: auth(adminKey),
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ keyScope: expect.any(String) });
  });

  it('404s an unknown route in the envelope', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/nope',
      headers: auth(readKey),
    });
    expect(res.statusCode).toBe(404);
    expect(res.json().error.code).toBe('NOT_FOUND');
  });

  /**
   * §12.1 / FR-13. Every other 429 assertion in this suite covers Riot's
   * upstream limiter, which is a different mechanism — so the consumer-quota
   * response went unchecked, and returned a 500 for as long as it has existed.
   */
  it('answers 429 QUOTA_EXCEEDED once a consumer spends its quota (§12.1)', async ({ skip }) => {
    if (!available || !app) return skip();
    const limited = await createTestConsumer({
      name: testConsumerName('quota'),
      scopes: ['read', 'admin'],
      quotaPerMin: 2,
    });
    // A route that costs nothing upstream: the quota is what is under test.
    const spend = () =>
      app!.inject({ method: 'GET', url: '/v1/admin/stats', headers: auth(limited?.key ?? '') });

    expect((await spend()).statusCode).toBe(200);
    expect((await spend()).statusCode).toBe(200);

    const denied = await spend();
    expect(denied.statusCode).toBe(429);
    expect(denied.json().error.code).toBe('QUOTA_EXCEEDED');
    // Without Retry-After a client has nothing to back off against.
    expect(Number(denied.headers['retry-after'])).toBeGreaterThan(0);
  });

  /**
   * Nothing else pins the hook ordering this depends on. `@fastify/rate-limit`
   * attaches its check via `onRoute`, so it runs as a route-level hook — after
   * the instance-level `onRequest` in auth/plugin.ts that sets
   * `request.consumer`. If the plugin ever moved to an instance-level hook the
   * limiter would see no consumer, silently fall back to keying by IP, and
   * every consumer sharing an address would share one quota.
   */
  it('meters each consumer separately rather than by address (§12.1)', async ({ skip }) => {
    if (!available || !app) return skip();
    const small = await createTestConsumer({
      name: testConsumerName('quota-small'),
      scopes: ['read', 'admin'],
      quotaPerMin: 2,
    });
    const large = await createTestConsumer({
      name: testConsumerName('quota-large'),
      scopes: ['read', 'admin'],
      quotaPerMin: 5,
    });
    const hit = (key: string) =>
      app!.inject({ method: 'GET', url: '/v1/admin/stats', headers: auth(key) });

    // Spend the small quota to exhaustion. inject() reports one client address,
    // so an IP-keyed bucket would now be spent for both consumers.
    await hit(small?.key ?? '');
    await hit(small?.key ?? '');
    expect((await hit(small?.key ?? '')).statusCode).toBe(429);

    const other = await hit(large?.key ?? '');
    expect(other.statusCode).toBe(200);
    // Its own limit, and its own untouched bucket — the IP fallback would show 60.
    expect(other.headers['x-ratelimit-limit']).toBe('5');
    expect(other.headers['x-ratelimit-remaining']).toBe('4');
  });

  it('creates and revokes a consumer through the admin surface (FR-14)', async ({ skip }) => {
    if (!available || !app) return skip();
    const created = await app.inject({
      method: 'POST',
      url: '/v1/admin/consumers',
      headers: auth(adminKey),
      payload: { name: testConsumerName('created-by-test'), quotaPerMin: 10 },
    });
    expect(created.statusCode).toBe(200);
    const body = created.json();
    expect(body.key).toMatch(/^rpx_/);
    // Revoking below is a soft delete, so this row still needs sweeping up.
    trackTestConsumer(body.id);

    const revoked = await app.inject({
      method: 'DELETE',
      url: `/v1/admin/consumers/${body.id}`,
      headers: auth(adminKey),
    });
    expect(revoked.statusCode).toBe(200);
  });

  it('purges the cache by pattern through the admin surface', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'POST',
      url: '/v1/admin/cache/purge',
      headers: auth(adminKey),
      payload: { pattern: 'summoner.byPuuid:*' },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ ok: true, deleted: expect.any(Number) });
  });
});

describe('admin IP allowlist (§12.1)', () => {
  it('permits everything when the allowlist is empty', () => {
    expect(ipAllowed('203.0.113.9', [])).toBe(true);
  });

  it('matches exact addresses and normalises IPv4-mapped IPv6', () => {
    expect(ipAllowed('127.0.0.1', ['127.0.0.1'])).toBe(true);
    expect(ipAllowed('::ffff:127.0.0.1', ['127.0.0.1'])).toBe(true);
    expect(ipAllowed('::1', ['::1'])).toBe(true);
    expect(ipAllowed('203.0.113.9', ['127.0.0.1'])).toBe(false);
  });

  it('matches CIDR blocks', () => {
    expect(ipAllowed('10.1.2.3', ['10.0.0.0/8'])).toBe(true);
    expect(ipAllowed('11.1.2.3', ['10.0.0.0/8'])).toBe(false);
    expect(ipAllowed('192.168.1.50', ['192.168.1.0/24'])).toBe(true);
    expect(ipAllowed('192.168.2.50', ['192.168.1.0/24'])).toBe(false);
  });

  it('rejects an unknown source when an allowlist is configured', () => {
    expect(ipAllowed(undefined, ['127.0.0.1'])).toBe(false);
  });
});

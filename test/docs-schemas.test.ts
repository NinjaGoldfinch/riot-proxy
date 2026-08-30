import './helpers/env.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { eq } from 'drizzle-orm';
import { buildApp, type App } from '../src/app.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { players } from '../src/db/schema.js';
import { listConsumers } from '../src/db/consumers.js';
import { listPlayers, upsertPlayer } from '../src/db/players.js';
import { createTestConsumer, removeTestConsumers, testConsumerName } from './helpers/consumers.js';
import { closeRedis, redis } from '../src/redis.js';
import { wsHub } from '../src/ws/index.js';

/**
 * Stage 1 of the docs work (#60) put response schemas on seven routes that had
 * none. That turns on `fast-json-stringify` for each of them, and a field the
 * schema does not name is not merely undocumented — it is dropped from the
 * response without an error. So these tests assert the *whole* body, and for
 * the two routes that serialise a database row they assert it against the keys
 * the query actually returns rather than against a hand-written list. A column
 * added to `consumers` or `players` and not added to the schema fails here.
 */
let app: App | undefined;
let adminKey = '';
let available = false;

const TEST_PUUID = `docs-schemas-${'x'.repeat(60)}`;

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const admin = await createTestConsumer({
    name: testConsumerName('docs'),
    scopes: ['read', 'admin'],
  });
  adminKey = admin?.key ?? '';

  // A player row has to exist for the list route to prove anything.
  await upsertPlayer({
    puuid: TEST_PUUID,
    platform: 'euw1',
    gameName: 'DocsFixture',
    tagLine: 'TEST',
    tracked: false,
  });

  app = await buildApp();
  await app.ready();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  if (available) {
    await db.delete(players).where(eq(players.puuid, TEST_PUUID));
    await removeTestConsumers();
  }
  await Promise.allSettled([closeRedis(), closeDb()]);
});

const auth = () => ({ authorization: `Bearer ${adminKey}` });

describe('health and ops schemas (#60)', () => {
  it('/healthz serialises exactly { ok }', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/healthz' });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ ok: true });
  });

  it('/readyz keeps every dependency flag', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/readyz' });
    expect(res.statusCode).toBe(200);
    const body = res.json();
    expect(Object.keys(body).sort()).toEqual(['keyScope', 'ok', 'postgres', 'redis']);
    expect(body).toMatchObject({ ok: true, redis: true, postgres: true });
    expect(typeof body.keyScope).toBe('string');
    expect(body.keyScope.length).toBeGreaterThan(0);
  });

  it('/metrics stays Prometheus text and is not JSON-encoded', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/metrics' });
    expect(res.statusCode).toBe(200);
    expect(res.headers['content-type']).toContain('text/plain');
    expect(res.body).toContain('proxy_requests_total');
    // The failure this guards: a `response: { 200: Type.String() }` would have
    // handed the body to fast-json-stringify and shipped a quoted string.
    expect(res.body.startsWith('"')).toBe(false);
  });
});

describe('admin list schemas (#60)', () => {
  it('serialises every consumer column except the key hash', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/v1/admin/consumers', headers: auth() });
    expect(res.statusCode).toBe(200);

    const [row] = await listConsumers();
    expect(row, 'no consumer rows to compare against').toBeDefined();

    const serialised = res.json().consumers.find((c: { id: string }) => c.id === row!.id);
    expect(
      serialised,
      'the consumer the query returned is missing from the response',
    ).toBeDefined();

    // The schema must name every column `listConsumers()` selects.
    expect(Object.keys(serialised).sort()).toEqual(Object.keys(row!).sort());
    expect(serialised).not.toHaveProperty('keyHash');
    expect(serialised.scopes).toEqual(expect.arrayContaining(['admin']));
    expect(typeof serialised.quotaPerMin).toBe('number');
    // Timestamps survive the Date -> ISO conversion rather than becoming {}.
    expect(serialised.createdAt).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    expect(serialised.disabledAt).toBeNull();
  });

  it('serialises every player column', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/tracked-players',
      headers: auth(),
    });
    expect(res.statusCode).toBe(200);

    const row = (await listPlayers()).find((p) => p.puuid === TEST_PUUID);
    expect(row, 'the fixture player is missing from the query').toBeDefined();

    const serialised = res
      .json()
      .players.find((p: { puuid: string }) => p.puuid === TEST_PUUID) as Record<string, unknown>;
    expect(serialised, 'the fixture player is missing from the response').toBeDefined();

    expect(Object.keys(serialised).sort()).toEqual(Object.keys(row!).sort());
    expect(serialised).toMatchObject({
      puuid: TEST_PUUID,
      platform: 'euw1',
      gameName: 'DocsFixture',
      tagLine: 'TEST',
      tracked: false,
    });
    // The nullable columns are emitted as null, not omitted.
    expect(serialised.lastSeenMatchId).toBeNull();
    expect(serialised.historyBackfilledAt).toBeNull();
    expect(serialised.historyBackfillDepth).toBeNull();
    expect(serialised.updatedAt).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  });

  it('/v1/admin/stats serialises exactly its three counters', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: '/v1/admin/stats', headers: auth() });
    expect(res.statusCode).toBe(200);
    const body = res.json();
    expect(Object.keys(body).sort()).toEqual(['archivedMatches', 'keyScope', 'trackedPlayers']);
    expect(typeof body.archivedMatches).toBe('number');
    expect(typeof body.trackedPlayers).toBe('number');
  });
});

describe('the limits scope param (#60)', () => {
  it('accepts a platform host', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/limits/euw1',
      headers: auth(),
    });
    expect(res.statusCode).toBe(200);
    expect(Object.keys(res.json()).sort()).toEqual(['frozenMs', 'scope', 'usage']);
    expect(res.json().scope).toBe('euw1');
  });

  it('accepts a region host too — a bucket is keyed by host, not by platform', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/limits/europe',
      headers: auth(),
    });
    expect(res.statusCode).toBe(200);
    expect(res.json().scope).toBe('europe');
  });

  it('rejects a scope that is neither', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: '/v1/admin/limits/not-a-host',
      headers: auth(),
    });
    // A behaviour change, and the intended one: before the params schema this
    // route took any string and answered 200 with the empty usage of a bucket
    // that does not exist.
    expect(res.statusCode).toBe(400);
    // `VALIDATION`, not `BAD_REGION`, and deliberately left that way. The
    // BAD_REGION branch in `toProxyError` keys off `/platform|region/i` in the
    // validation message, and this param is named `scope`. Widening that regex
    // to catch it would also catch `scopes` on `POST /v1/admin/consumers`,
    // where a bad permission array reported as a bad region is worse than the
    // generic code is here.
    expect(res.json().error.code).toBe('VALIDATION');
  });
});

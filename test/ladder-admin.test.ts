import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { eq, inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import type { App } from '../src/app.js';

/**
 * The admin surface for the crawl (#88), against the real Postgres and Redis.
 *
 * Those two are real because the trigger route's whole job is to reconcile "a
 * crawl is already running" — a fact held by a partial unique index — with
 * what the caller is told, and because the outstanding-leg set the cancel
 * route clears lives in Redis.
 *
 * The queue is emphatically **not** real. A developer with `npm run dev:worker`
 * running shares this Redis, so a test that queued a genuine crawl would have
 * that worker enumerate an actual ladder against an actual Riot key — the
 * suite would spend someone's quota by existing. `enqueueBackfill` takes an
 * injectable queue for the milder version of this problem; here the stakes are
 * high enough to swap the queue out entirely.
 */
const jobs: FakeJob[] = [];
let nextJobId = 1;

interface FakeJob {
  id: string;
  name: string;
  data: Record<string, unknown>;
  opts: Record<string, unknown>;
  remove: () => Promise<void>;
}

vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    ladderQueue: {
      addBulk: async (added: { name: string; data: unknown; opts: unknown }[]) => {
        for (const job of added) {
          const id = String(nextJobId++);
          jobs.push({
            id,
            name: job.name,
            data: job.data as Record<string, unknown>,
            opts: job.opts as Record<string, unknown>,
            remove: async () => {
              const i = jobs.findIndex((j) => j.id === id);
              if (i === -1) throw new Error('already removed');
              jobs.splice(i, 1);
            },
          });
        }
        return added;
      },
      getJobs: async () => [...jobs],
    },
  };
});

const { buildApp } = await import('../src/app.js');
const { closeDb, db, pingDb } = await import('../src/db/index.js');
const { ladderCrawls, leagueEntries } = await import('../src/db/schema.js');
const { getCrawl } = await import('../src/db/ladder.js');
const { pendingLegs } = await import('../src/jobs/ladder-state.js');
const { createTestConsumer, removeTestConsumers, testConsumerName } =
  await import('./helpers/consumers.js');
const { closeRedis, redis } = await import('../src/redis.js');
const { KEY_SCOPE, config } = await import('../src/config.js');
const { wsHub } = await import('../src/ws/index.js');

/**
 * `oc1` rather than the default platform: one live crawl per
 * (key_scope, platform, queue) is enforced by the database, so a suite sharing
 * a platform with a developer's own crawl would block it for real.
 */
const PLATFORM = 'oc1';
/** A second ladder, for the rule that a crawl of one does not block another. */
const OTHER_PLATFORM = 'ru';
const QUEUE = 'RANKED_SOLO_5x5';

let app: App | undefined;
let adminKey = '';
let readKey = '';
let available = false;

const auth = (key: string) => ({ authorization: `Bearer ${key}` });

async function wipe(): Promise<void> {
  await db.delete(leagueEntries).where(inArray(leagueEntries.platform, [PLATFORM, OTHER_PLATFORM]));
  await db.delete(ladderCrawls).where(inArray(ladderCrawls.platform, [PLATFORM, OTHER_PLATFORM]));
  jobs.length = 0;

  // A crawl this suite starts and never finishes leaves its outstanding-leg
  // set behind for the key's whole TTL. Harmless — the scope is the pinned
  // test key, not a developer's — but the limiter suite cleans its own keys up
  // on the way out and this one should too, rather than leaving a day of
  // debris in a Redis someone is also developing against.
  let cursor = '0';
  do {
    const [next, keys] = await redis.scan(cursor, 'MATCH', `ladder:*:${KEY_SCOPE}:*`, 'COUNT', 200);
    cursor = next;
    if (keys.length > 0) await redis.del(...keys);
  } while (cursor !== '0');
}

beforeAll(async () => {
  available = await probeServices('ladder-admin.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (!available) return;

  const admin = await createTestConsumer({
    name: testConsumerName('ladder-admin'),
    scopes: ['read', 'admin'],
  });
  const read = await createTestConsumer({
    name: testConsumerName('ladder-read'),
    scopes: ['read'],
  });
  adminKey = admin?.key ?? '';
  readKey = read?.key ?? '';

  app = await buildApp();
  await app.ready();
});

beforeEach(async () => {
  if (available) await wipe();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  if (available) {
    await wipe();
    await removeTestConsumers();
  }
  await Promise.allSettled([closeRedis(), closeDb()]);
});

const trigger = (body: Record<string, unknown>, key = adminKey) =>
  app!.inject({ method: 'POST', url: '/v1/admin/ladder/crawl', headers: auth(key), payload: body });

describe('POST /v1/admin/ladder/crawl', () => {
  it('answers 202 with the crawl id and how many legs it fanned out', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });

    expect(res.statusCode).toBe(202);
    const body = res.json() as { crawlId: string; status: string; legs: number };
    expect(body.status).toBe('started');
    // MASTER floor is apex-only: three requests, no paged walk at all.
    expect(body.legs).toBe(3);

    const crawl = await getCrawl(body.crawlId);
    expect(crawl?.status).toBe('running');
    expect(crawl?.platform).toBe(PLATFORM);
    expect(crawl?.tierFloor).toBe('MASTER');
    expect(await pendingLegs(body.crawlId)).toBe(3);
  });

  it('queues one job per leg, each with an explicit priority', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    const { crawlId } = res.json() as { crawlId: string };

    const mine = jobs.filter((j) => j.data['crawlId'] === crawlId);
    // Three apex leagues plus DIAMOND I–IV.
    expect(mine).toHaveLength(7);
    expect(mine.every((j) => typeof j.opts.priority === 'number')).toBe(true);
  });

  it('hands a second trigger the crawl already running instead of starting one', async ({
    skip,
  }) => {
    if (!available || !app) return skip();
    const first = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' })
    ).json() as {
      crawlId: string;
    };
    const second = await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'IRON' });

    // 202, not 409: nothing failed. The caller polls the same id either way,
    // and `status` is what distinguishes the two.
    expect(second.statusCode).toBe(202);
    const body = second.json() as Record<string, unknown>;
    // The ladder is named, so "already running" is checkable rather than an
    // id the caller has to look up to find out which ladder it is about.
    expect(body).toEqual({
      crawlId: first.crawlId,
      status: 'already-running',
      platform: PLATFORM,
      queue: QUEUE,
      legs: 0,
    });

    const rows = await db.select().from(ladderCrawls).where(eq(ladderCrawls.platform, PLATFORM));
    expect(rows).toHaveLength(1);
  });

  it('starts a crawl on another ladder while this one is running', async ({ skip }) => {
    if (!available || !app) return skip();
    // "One live crawl per ladder" is per (key_scope, platform, queue), and a
    // trigger told the wrong one is running is worse than a refusal: it hands
    // back an id for a ladder the caller did not ask about.
    const first = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' })
    ).json() as { crawlId: string };

    const otherPlatform = await trigger({
      platform: OTHER_PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
    });
    const otherQueue = await trigger({
      platform: PLATFORM,
      queue: 'RANKED_FLEX_SR',
      tierFloor: 'MASTER',
    });

    expect(otherPlatform.json()).toMatchObject({
      status: 'started',
      platform: OTHER_PLATFORM,
      queue: QUEUE,
    });
    expect(otherQueue.json()).toMatchObject({
      status: 'started',
      platform: PLATFORM,
      queue: 'RANKED_FLEX_SR',
    });
    for (const res of [otherPlatform, otherQueue]) {
      expect((res.json() as { crawlId: string }).crawlId).not.toBe(first.crawlId);
    }
  });

  it('rejects a tier floor and a queue Riot does not serve', async ({ skip }) => {
    if (!available || !app) return skip();
    for (const body of [
      { platform: PLATFORM, queue: QUEUE, tierFloor: 'WOOD' },
      { platform: PLATFORM, queue: 'RANKED_TFT_TURBO' },
      { platform: 'not-a-platform', queue: QUEUE },
    ]) {
      const res = await trigger(body);
      expect(res.statusCode, JSON.stringify(body)).toBe(400);
    }
  });

  it('needs an admin key', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await trigger({ platform: PLATFORM, queue: QUEUE }, readKey);
    expect(res.statusCode).toBe(403);
  });
});

describe('GET /v1/admin/ladder/crawls', () => {
  it('lists runs newest first with their counters and what is still outstanding', async ({
    skip,
  }) => {
    if (!available || !app) return skip();
    const { crawlId } = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' })
    ).json() as { crawlId: string };

    const res = await app.inject({
      method: 'GET',
      url: `/v1/admin/ladder/crawls?platform=${PLATFORM}`,
      headers: auth(adminKey),
    });

    expect(res.statusCode).toBe(200);
    const { crawls } = res.json() as { crawls: Record<string, unknown>[] };
    expect(crawls).toHaveLength(1);
    expect(crawls[0]).toMatchObject({
      id: crawlId,
      platform: PLATFORM,
      queue: QUEUE,
      status: 'running',
      // A crawl starts by walking the ladder; nothing it discovers is fetched
      // until every leg of that stage is in.
      phase: 'enumerate',
      pagesFetched: 0,
      entriesSeen: 0,
      matchIdsSeen: 0,
      matchesQueued: 0,
      pendingLegs: 3,
    });
    expect(crawls[0]?.['finishedAt']).toBeNull();
  });
});

describe('GET /v1/admin/ladder/options', () => {
  const options = (key = adminKey) =>
    app!.inject({ method: 'GET', url: '/v1/admin/ladder/options', headers: auth(key) });

  it('offers the ladders a crawl can name, and this deployment’s defaults', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await options();

    expect(res.statusCode).toBe(200);
    const body = res.json() as {
      platforms: { id: string; label: string }[];
      queues: string[];
      tiers: string[];
      defaults: Record<string, unknown>;
    };

    // The dashboard's form is built from this, so a value it offers has to be
    // one the trigger route accepts — same enums, one source.
    expect(body.platforms.map((p) => p.id)).toContain(PLATFORM);
    expect(body.platforms.every((p) => p.label.length > 0)).toBe(true);
    expect(body.queues).toContain(QUEUE);
    expect(body.tiers).toEqual([
      'IRON',
      'BRONZE',
      'SILVER',
      'GOLD',
      'PLATINUM',
      'EMERALD',
      'DIAMOND',
      'MASTER',
      'GRANDMASTER',
      'CHALLENGER',
    ]);
    expect(body.defaults).toMatchObject({
      platform: config.DEFAULT_PLATFORM,
      tierFloor: config.ladderTierFloor,
      backfillLimit: config.LADDER_BACKFILL_LIMIT,
    });

    // The form's own defaults, submitted — on this suite's platform rather
    // than the configured one, which a developer's real crawl may hold.
    const started = await trigger({
      platform: PLATFORM,
      queue: body.defaults['queue'] as string,
      tierFloor: body.defaults['tierFloor'] as string,
    });
    expect(started.statusCode).toBe(202);
  });

  it('needs the admin scope like everything else here', async ({ skip }) => {
    if (!available || !app) return skip();
    expect((await options(readKey)).statusCode).toBe(403);
  });
});

describe('DELETE /v1/admin/ladder/crawls/:id', () => {
  const cancel = (id: string) =>
    app!.inject({
      method: 'DELETE',
      url: `/v1/admin/ladder/crawls/${id}`,
      headers: auth(adminKey),
    });

  it('marks the crawl cancelled and drops the legs still queued', async ({ skip }) => {
    if (!available || !app) return skip();
    const { crawlId } = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' })
    ).json() as { crawlId: string };

    const res = await cancel(crawlId);
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ ok: true, crawlId, status: 'cancelled', droppedJobs: 7 });

    expect((await getCrawl(crawlId))?.status).toBe('cancelled');
    expect(jobs.filter((j) => j.data['crawlId'] === crawlId)).toHaveLength(0);
    // And the Redis state goes with it, so nothing is left to finish a crawl
    // that has already ended.
    expect(await pendingLegs(crawlId)).toBe(0);
  });

  it('frees the ladder for the next crawl', async ({ skip }) => {
    if (!available || !app) return skip();
    const first = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' })
    ).json() as { crawlId: string };
    await cancel(first.crawlId);

    const second = await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    const body = second.json() as { crawlId: string; status: string };
    expect(body.status).toBe('started');
    expect(body.crawlId).not.toBe(first.crawlId);
  });

  it('refuses to cancel a crawl that has already ended', async ({ skip }) => {
    if (!available || !app) return skip();
    const { crawlId } = (
      await trigger({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' })
    ).json() as { crawlId: string };
    await cancel(crawlId);

    const again = await cancel(crawlId);
    expect(again.statusCode).toBe(400);
    expect((again.json() as { error: { message: string } }).error.message).toContain('cancelled');
  });

  it('404s for a crawl that does not exist', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await cancel('00000000-0000-4000-8000-00000000dead');
    expect(res.statusCode).toBe(404);
  });
});

import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { Value } from '@sinclair/typebox/value';
import './helpers/formats.js';
import { eq } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { ladderCrawls, leagueEntries } from '../src/db/schema.js';
import { createCrawl, finishCrawl, bumpCrawlCounters } from '../src/db/ladder.js';
import { clearCrawlState, trackLegs } from '../src/jobs/ladder-state.js';
import { QUEUE_NAMES, closeQueues } from '../src/jobs/queues.js';
import { closeRedis, redis } from '../src/redis.js';
import { MetricsSnapshot } from '../src/stats/schema.js';
import { buildMetricsSnapshot } from '../src/stats/snapshot.js';
import {
  ladderCrawlDuration,
  ladderEntriesTotal,
  ladderPagesTotal,
  registry,
} from '../src/metrics.js';

/**
 * Watching a crawl while it runs (#91).
 *
 * A multi-hour bulk job that shares its rate-limit budget with interactive
 * traffic has to be observable, not merely auditable afterwards — and the
 * queue table on the dashboard cannot tell a crawl doing its job from
 * something stuck, because both look like a backlog.
 */
const PLATFORM = 'ladder-obs-test';
const QUEUE = 'RANKED_SOLO_5x5';
/** A label set no real crawl can produce, so the probes cannot mask a bug. */
const PROBE = { platform: 'ladder-obs-probe', queue: QUEUE };

let available = false;

const inputs = { wsConnections: 0, wsSubscriptions: 0, wsEventCounts: {} };

async function wipe(): Promise<void> {
  const rows = await db
    .select({ id: ladderCrawls.id })
    .from(ladderCrawls)
    .where(eq(ladderCrawls.platform, PLATFORM));
  for (const row of rows) await clearCrawlState(row.id);
  await db.delete(leagueEntries).where(eq(leagueEntries.platform, PLATFORM));
  await db.delete(ladderCrawls).where(eq(ladderCrawls.platform, PLATFORM));
}

beforeAll(async () => {
  available = await probeServices('ladder-observability.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (available) await wipe();
});

beforeEach(async () => {
  if (available) await wipe();
});

afterAll(async () => {
  if (available) await wipe();
  await Promise.allSettled([closeQueues(), closeRedis(), closeDb()]);
});

describe('the metrics registry', () => {
  it('registers the three ladder metrics §8 asks for', async () => {
    const names = (await registry.getMetricsAsJSON()).map((m) => m.name);
    expect(names).toContain('proxy_ladder_pages_total');
    expect(names).toContain('proxy_ladder_entries_total');
    expect(names).toContain('proxy_ladder_crawl_duration_seconds');
  });

  it('buckets crawl duration in minutes and hours, not seconds', async () => {
    // A full ladder on a dev key is five to seven hours (§2), and the default
    // prom-client buckets top out at ten seconds — every real crawl would land
    // in `+Inf` and the histogram would answer nothing.
    ladderCrawlDuration.observe({ ...PROBE, status: 'completed' }, 3600);
    // `le` is prom-client's own bucket label and is not in the declared label
    // union, so the rows are read as plain records here.
    const values = (
      (await ladderCrawlDuration.get()).values as {
        labels: Record<string, string | number>;
        value: number;
      }[]
    ).filter((v) => v.labels['platform'] === PROBE.platform);

    const hourly = values.find((v) => String(v.labels['le']) === '3600');
    expect(hourly?.value, 'an hour-long crawl must land in a real bucket').toBe(1);
    const finite = values.map((v) => Number(v.labels['le'])).filter((n) => Number.isFinite(n));
    expect(Math.max(...finite)).toBeGreaterThanOrEqual(7200);
  });

  it('labels pages and entries by ladder, since two crawls share one budget', async () => {
    ladderPagesTotal.inc(PROBE);
    ladderEntriesTotal.inc(PROBE, 205);

    const pages = (await ladderPagesTotal.get()).values.find(
      (v) => v.labels['platform'] === PROBE.platform,
    );
    const entries = (await ladderEntriesTotal.get()).values.find(
      (v) => v.labels['platform'] === PROBE.platform,
    );

    // Both dimensions, because a deployment crawling two ladders shares one
    // rate-limit budget between them and needs to see which is spending it.
    expect(pages?.labels).toEqual(PROBE);
    expect(pages?.value).toBe(1);
    expect(entries?.value).toBe(205);
  });
});

describe('the ladder block in the snapshot', () => {
  it('still satisfies the schema both transports promise', async ({ skip }) => {
    if (!available) return skip();
    const snap = await buildMetricsSnapshot(inputs);
    expect([...Value.Errors(MetricsSnapshot, snap)]).toEqual([]);
    expect(snap.ladder).toBeDefined();
    expect(Array.isArray(snap.ladder.running)).toBe(true);
  });

  it('shows a running crawl with the legs it has left', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({
      platform: PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
    });
    await trackLegs(crawl.id, ['leg-a', 'leg-b', 'leg-c']);
    await bumpCrawlCounters(crawl.id, {
      pagesFetched: 2,
      entriesSeen: 300,
      playersDiscovered: 40,
      backfillsEnqueued: 40,
    });

    const snap = await buildMetricsSnapshot(inputs);
    const mine = snap.ladder.running.find((c) => c.id === crawl.id);

    expect(mine).toBeDefined();
    expect(mine).toMatchObject({
      platform: PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
      status: 'running',
      pagesFetched: 2,
      entriesSeen: 300,
      playersDiscovered: 40,
      backfillsEnqueued: 40,
      finishedAt: null,
    });
    // The number that moves while a multi-hour walk sits between pages, and
    // the reason the block reads Redis rather than only the row.
    expect(mine?.pendingLegs).toBe(3);
  });

  it('moves a finished crawl to lastCompleted, however it ended', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({
      platform: PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
    });
    await finishCrawl(crawl.id, 'failed');

    const snap = await buildMetricsSnapshot(inputs);
    expect(snap.ladder.running.find((c) => c.id === crawl.id)).toBeUndefined();
    // "However it ended" on purpose: a failed run is the one an operator most
    // needs to see, and hiding it behind a `completed` filter would mean the
    // dashboard shows nothing at all after a bad night.
    expect(snap.ladder.lastCompleted?.id).toBe(crawl.id);
    expect(snap.ladder.lastCompleted?.status).toBe('failed');
    expect(snap.ladder.lastCompleted?.finishedAt).not.toBeNull();
    // Its legs were cleared, so the read is skipped rather than reported as an
    // exact zero from a key that no longer exists.
    expect(snap.ladder.lastCompleted?.pendingLegs).toBe(0);
  });

  it('counts the ladder this key scope holds', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({
      platform: PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
    });
    const before = (await buildMetricsSnapshot(inputs)).ladder.entries;

    await db.insert(leagueEntries).values({
      keyScope: (await import('../src/config.js')).KEY_SCOPE,
      platform: PLATFORM,
      queue: QUEUE,
      puuid: 'ladder-obs-puuid',
      tier: 'MASTER',
      division: 'I',
      leaguePoints: 0,
      wins: 0,
      losses: 0,
      firstSeenCrawlId: crawl.id,
      lastSeenCrawlId: crawl.id,
    });

    expect((await buildMetricsSnapshot(inputs)).ladder.entries).toBe(before + 1);
  });

  it('carries the ladder queue alongside the depths a crawl inflates', async ({ skip }) => {
    if (!available) return skip();
    const snap = await buildMetricsSnapshot(inputs);
    // The dashboard's queue table is generated from this record, so the new
    // queue appears there without any dashboard change.
    expect(Object.keys(snap.queues)).toContain(QUEUE_NAMES.ladder);
  });
});

import type { Redis } from 'ioredis';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
import {
  counter,
  forgetJob,
  get,
  jobIdsInState,
  metrics,
  percentile,
  post,
  redisClient,
  sleep,
  timed,
  waitFor,
} from './helpers/harness.js';

/**
 * Phase 5 — a backfill must fill the archive, cost Riot nothing on re-request,
 * and stay out of the way of interactive traffic while it runs (bulk priority
 * behind BULK_USAGE_CEILING, §9.3).
 */
const enabled = acceptance.enabled;

interface Stats {
  keyScope: string;
  archivedMatches: number;
  trackedPlayers: number;
}

let puuid = '';
let redis: Redis;
let matchIds: string[] = [];

async function archivedCount(): Promise<number> {
  return (await get<Stats>('/v1/admin/stats')).body.archivedMatches;
}

async function queuesIdle(): Promise<boolean> {
  for (const queue of ['backfill', 'archive'] as const) {
    for (const state of ['wait', 'active', 'delayed'] as const) {
      if ((await jobIdsInState(redis, queue, state)).length > 0) return false;
    }
  }
  return true;
}

describe.skipIf(!enabled)('Phase 5 — archive and backfill', () => {
  beforeAll(async () => {
    const { gameName, tagLine, region } = cfg();
    redis = redisClient();
    const account = await get<{ puuid: string }>(
      `/v1/riot/accounts/by-riot-id/${region}/${encodeURIComponent(gameName)}/${encodeURIComponent(tagLine)}`,
    );
    puuid = account.body.puuid;
  });

  afterAll(async () => {
    await redis?.quit();
  });

  it('backfills history without degrading interactive latency', async () => {
    const { platform, backfillLimit } = cfg();
    const probe = `/v1/lol/status/${platform}`;

    // Warm the probe so the baseline measures a cache hit, not a cold fetch.
    await timed(probe);
    const baseline: number[] = [];
    for (let i = 0; i < 10; i++) baseline.push(await timed(probe));

    const startedAt = await archivedCount();
    const upstreamBefore = await metrics();

    // BullMQ dedupes on job id and retains finished jobs, so a repeat run is
    // silently dropped unless the previous one is forgotten first.
    await forgetJob(redis, 'backfill', `backfill-${puuid}`);
    const enqueued = await post<{ ok: boolean; jobId: string }>('/v1/admin/backfill', {
      puuid,
      platform,
      limit: backfillLimit,
    });
    expect(enqueued.status, JSON.stringify(enqueued.body)).toBe(200);
    expect(enqueued.body.ok).toBe(true);

    // Sample interactive latency while the bulk work is actually in flight.
    const during: number[] = [];
    const sampler = (async () => {
      while (!(await queuesIdle())) {
        during.push(await timed(probe));
        await sleep(500);
      }
    })();

    await waitFor(
      'the backfill and archive queues to drain',
      async () => ((await queuesIdle()) ? true : undefined),
      { timeoutMs: 10 * 60_000, intervalMs: 2000 },
    );
    await sampler;

    for (const queue of ['backfill', 'archive'] as const) {
      const failed = await jobIdsInState(redis, queue, 'failed');
      expect(failed, `${queue} jobs failed: ${failed.join(', ')}`).toEqual([]);
    }

    const endedAt = await archivedCount();
    // Did the backfill actually reach Riot? On a warm archive it legitimately
    // has nothing to do, and a pass then proves nothing about pacing — say so
    // rather than reporting a green tick.
    const fetched =
      counter(await metrics(), 'proxy_upstream_requests_total', { method: 'match.byId' }) -
      counter(upstreamBefore, 'proxy_upstream_requests_total', { method: 'match.byId' });

    process.stdout.write(
      `  phase 5: archive ${startedAt} -> ${endedAt}, ${fetched} fetched from Riot, ` +
        `${during.length} latency samples\n`,
    );

    if (fetched === 0) {
      process.stdout.write(
        '  phase 5: INCONCLUSIVE — every match was already archived, so nothing was paced.\n' +
          '           Re-run against a fresh database, or raise ACCEPTANCE_BACKFILL_LIMIT\n' +
          '           past the archived depth, to exercise the bulk path.\n',
      );
      return;
    }

    // Every match Riot served must have landed in the archive.
    expect(endedAt - startedAt).toBe(fetched);

    if (during.length < 5) {
      process.stdout.write('  phase 5: backfill finished too fast to judge latency\n');
      return;
    }
    const basep95 = percentile(baseline, 95);
    const duringp95 = percentile(during, 95);
    process.stdout.write(
      `  phase 5: interactive p95 ${basep95.toFixed(1)}ms idle -> ${duringp95.toFixed(1)}ms during\n`,
    );
    // Interactive reads are cache hits either way; the absolute floor matters
    // more than the ratio when the baseline is already sub-millisecond.
    expect(duringp95).toBeLessThanOrEqual(Math.max(basep95 * 4, basep95 + 250));
  });

  it('serves every archived match without touching Riot (FR-8)', async () => {
    const { region } = cfg();
    const ids = await get<string[]>(`/v1/lol/matches/ids/${region}/${puuid}?count=20`);
    matchIds = ids.body;
    expect(matchIds.length).toBeGreaterThan(0);

    // Prime anything the backfill did not reach, so the measured pass below is
    // purely about re-reads.
    for (const id of matchIds) await get(`/v1/lol/matches/${region}/${id}`);

    const before = await metrics();
    for (const id of matchIds) {
      const res = await get<{ metadata: { matchId: string } }>(`/v1/lol/matches/${region}/${id}`);
      expect(res.body.metadata.matchId).toBe(id);
    }
    const after = await metrics();

    for (const method of ['match.byId', 'match.timeline'] as const) {
      const upstream =
        counter(after, 'proxy_upstream_requests_total', { method }) -
        counter(before, 'proxy_upstream_requests_total', { method });
      expect(upstream, `${method} must stay flat across ${matchIds.length} re-reads`).toBe(0);
    }
  });
});

import type { Job } from 'bullmq';
import { KEY_SCOPE } from '../config.js';
import { config } from '../config.js';
import { archiveMatch, archiveTimeline, filterUnarchived, type RiotMatch } from '../db/matches.js';
import { listTrackedPlayers, setLastSeenMatch } from '../db/players.js';
import { PATCH_TOPIC, playerTopic, publish } from '../events/index.js';
import { fetcher } from '../fetcher.js';
import { ProxyError } from '../errors.js';
import { logger } from '../logger.js';
import { jobsTotal } from '../metrics.js';
import { redis } from '../redis.js';
import { build } from '../riot/endpoints.js';
import { assertPlatform, platformToRegion, regionFromMatchId } from '../riot/routing.js';
import { syncDdragon } from '../static/ddragon.js';
import {
  JOB,
  archiveQueue,
  pollQueue,
  type ArchiveMatchJob,
  type BackfillPlayerJob,
  type PollPlayerJob,
} from './queues.js';

/**
 * §10 — every job is idempotent and every upstream call it makes runs at `bulk`
 * priority so backfills cannot starve interactive traffic (§9.3).
 */
const BULK = { priority: 'bulk' as const };

/** Previous-state keys for transition detection (Phase 6.2). */
const stateKey = {
  live: (puuid: string) => `state:live:${KEY_SCOPE}:${puuid}`,
  rank: (puuid: string) => `state:rank:${KEY_SCOPE}:${puuid}`,
};

const STATE_TTL = 7 * 24 * 3600;

// ── fan-out ticks ────────────────────────────────────────────────────────────

/**
 * One repeatable tick per poll type fans out to one job per tracked player.
 * Cheaper than maintaining N repeatable jobs, and tracked-player changes take
 * effect on the next tick with no scheduler churn.
 */
async function fanOut(jobName: string): Promise<number> {
  const players = await listTrackedPlayers();
  if (players.length === 0) return 0;

  await pollQueue.addBulk(
    players.map((p) => ({
      name: jobName,
      data: { puuid: p.puuid, platform: p.platform } satisfies PollPlayerJob,
      opts: {
        // De-duplicate: if the previous tick's job for this player is still
        // queued, do not stack another one on top of it.
        jobId: `${jobName}:${p.puuid}:${Math.floor(Date.now() / 1000)}`,
        removeOnComplete: { age: 600, count: 200 },
      },
    })),
  );
  return players.length;
}

// ── poll:live ────────────────────────────────────────────────────────────────

interface ActiveGame {
  gameId?: number;
  gameQueueConfigId?: number;
  participants?: { puuid?: string; championId?: number }[];
}

export async function pollLive(job: Job<PollPlayerJob>): Promise<void> {
  const { puuid, platform } = job.data;
  const key = stateKey.live(puuid);
  const previous = await redis.get(key);

  let game: ActiveGame | undefined;
  try {
    const result = await fetcher.fetch<ActiveGame>(
      build.activeGame(assertPlatform(platform), puuid),
      BULK,
    );
    game = result.data;
  } catch (err) {
    // 404 is the normal "not in game" answer, negative-cached upstream (§8.3).
    if (!(err instanceof ProxyError && err.code === 'NOT_FOUND')) throw err;
  }

  const currentGameId = game?.gameId ? String(game.gameId) : undefined;

  if (currentGameId && currentGameId !== previous) {
    const self = game?.participants?.find((p) => p.puuid === puuid);
    await redis.set(key, currentGameId, 'EX', STATE_TTL);
    await publish('game.started', playerTopic(puuid), {
      puuid,
      platform,
      gameId: Number(currentGameId),
      ...(self?.championId !== undefined ? { championId: self.championId } : {}),
      ...(game?.gameQueueConfigId !== undefined ? { queueId: game.gameQueueConfigId } : {}),
    });
    return;
  }

  if (!currentGameId && previous) {
    await redis.del(key);
    await publish('game.ended', playerTopic(puuid), { puuid, gameId: Number(previous) });
    // The match becomes available shortly after the game ends; nudge the match
    // poller rather than waiting for its next tick.
    await pollQueue.add(JOB.pollMatches, { puuid, platform } satisfies PollPlayerJob, {
      delay: 60_000,
    });
  }
}

// ── poll:rank ────────────────────────────────────────────────────────────────

interface LeagueEntry {
  queueType?: string;
  tier?: string;
  rank?: string;
  leaguePoints?: number;
}

type RankSnapshot = Record<string, { tier?: string; rank?: string; lp?: number }>;

export async function pollRank(job: Job<PollPlayerJob>): Promise<void> {
  const { puuid, platform } = job.data;
  const key = stateKey.rank(puuid);

  const { data: entries } = await fetcher.fetch<LeagueEntry[]>(
    build.leagueEntriesByPuuid(assertPlatform(platform), puuid),
    BULK,
  );

  const snapshot: RankSnapshot = {};
  for (const entry of entries ?? []) {
    if (!entry.queueType) continue;
    snapshot[entry.queueType] = {
      ...(entry.tier !== undefined ? { tier: entry.tier } : {}),
      ...(entry.rank !== undefined ? { rank: entry.rank } : {}),
      ...(entry.leaguePoints !== undefined ? { lp: entry.leaguePoints } : {}),
    };
  }

  const previousRaw = await redis.get(key);
  await redis.set(key, JSON.stringify(snapshot), 'EX', STATE_TTL);

  if (!previousRaw) return; // First observation is a baseline, not a change.

  let previous: RankSnapshot;
  try {
    previous = JSON.parse(previousRaw) as RankSnapshot;
  } catch {
    return;
  }

  for (const [queue, after] of Object.entries(snapshot)) {
    const before = previous[queue] ?? null;
    if (JSON.stringify(before) === JSON.stringify(after)) continue;
    await publish('rank.changed', playerTopic(puuid), { puuid, queue, before, after });
  }
}

// ── poll:matches ─────────────────────────────────────────────────────────────

export async function pollMatches(job: Job<PollPlayerJob>): Promise<void> {
  const { puuid, platform } = job.data;
  const region = platformToRegion(assertPlatform(platform));

  const { data: ids } = await fetcher.fetch<string[]>(
    build.matchIdsByPuuid(region, puuid, { start: 0, count: 5 }),
    BULK,
  );
  if (!ids || ids.length === 0) return;

  const unarchived = await filterUnarchived(ids);
  if (unarchived.length === 0) return;

  await archiveQueue.addBulk(
    unarchived.map((matchId) => ({
      name: JOB.archiveMatch,
      data: { matchId, puuid, fetchTimeline: config.ARCHIVE_TIMELINES } satisfies ArchiveMatchJob,
      // Idempotency: the same match never queues twice.
      opts: { jobId: `archive:${matchId}` },
    })),
  );

  const newest = ids[0];
  if (newest) await setLastSeenMatch(puuid, newest);
}

// ── archive:match ────────────────────────────────────────────────────────────

export async function archiveMatchJob(job: Job<ArchiveMatchJob>): Promise<void> {
  const { matchId, puuid, fetchTimeline } = job.data;
  const region = regionFromMatchId(matchId);
  if (!region) throw new Error(`Cannot derive region from match id '${matchId}'`);

  const { data } = await fetcher.fetch<RiotMatch>(build.matchById(region, matchId), BULK);
  await archiveMatch(matchId, region, data);

  if (fetchTimeline) {
    try {
      const timeline = await fetcher.fetch<unknown>(build.matchTimeline(region, matchId), BULK);
      await archiveTimeline(matchId, region, timeline.data);
    } catch (err) {
      // Timelines are large and optional — never fail the archive over one.
      logger.warn({ err, matchId }, 'timeline archive failed');
    }
  }

  await publish('match.archived', playerTopic(puuid ?? ''), {
    ...(puuid ? { puuid } : {}),
    matchId,
  });
}

// ── backfill:player ──────────────────────────────────────────────────────────

const BACKFILL_PAGE = 100;

export async function backfillPlayer(job: Job<BackfillPlayerJob>): Promise<{ queued: number }> {
  const { puuid, platform, limit = 500, fetchTimeline } = job.data;
  const region = platformToRegion(assertPlatform(platform));

  let start = 0;
  let queued = 0;

  while (start < limit) {
    const count = Math.min(BACKFILL_PAGE, limit - start);
    const { data: ids } = await fetcher.fetch<string[]>(
      build.matchIdsByPuuid(region, puuid, { start, count }),
      BULK,
    );
    if (!ids || ids.length === 0) break;

    const unarchived = await filterUnarchived(ids);
    if (unarchived.length > 0) {
      await archiveQueue.addBulk(
        unarchived.map((matchId) => ({
          name: JOB.archiveMatch,
          data: {
            matchId,
            puuid,
            fetchTimeline: fetchTimeline ?? config.ARCHIVE_TIMELINES,
          } satisfies ArchiveMatchJob,
          opts: { jobId: `archive:${matchId}`, priority: 10 },
        })),
      );
      queued += unarchived.length;
    }

    await job.updateProgress(Math.min(100, Math.round(((start + ids.length) / limit) * 100)));
    if (ids.length < count) break; // Reached the end of this player's history.
    start += ids.length;
  }

  logger.info({ puuid, queued }, 'backfill complete');
  return { queued };
}

// ── ddragon:sync ─────────────────────────────────────────────────────────────

export async function ddragonSync(job: Job<{ force?: boolean }>): Promise<void> {
  const result = await syncDdragon({ force: job.data?.force ?? false });
  if (!result.changed) return;

  // FR-11: bust the static cache and tell subscribers a patch landed.
  await publish('patch.new', PATCH_TOPIC, { version: result.version });
}

// ── maintenance ──────────────────────────────────────────────────────────────

/**
 * §10 — daily housekeeping. Negative keys and quota counters carry their own
 * TTLs, so this is mostly a safety net against orphaned single-flight locks
 * left behind by a hard crash.
 */
export async function maintenance(): Promise<{ locksCleared: number }> {
  let cursor = '0';
  let cleared = 0;
  do {
    const [next, keys] = await redis.scan(cursor, 'MATCH', 'sf:*', 'COUNT', 500);
    cursor = next;
    for (const key of keys) {
      const ttl = await redis.pttl(key);
      // -1 means "no expiry": a lock that outlived its PX, i.e. a leak.
      if (ttl === -1) {
        await redis.del(key);
        cleared += 1;
      }
    }
  } while (cursor !== '0');

  logger.info({ locksCleared: cleared }, 'maintenance complete');
  return { locksCleared: cleared };
}

// ── dispatch ─────────────────────────────────────────────────────────────────

export async function dispatch(job: Job): Promise<unknown> {
  try {
    let result: unknown;
    switch (job.name) {
      case JOB.pollLiveTick:
        result = { fannedOut: await fanOut(JOB.pollLive) };
        break;
      case JOB.pollRankTick:
        result = { fannedOut: await fanOut(JOB.pollRank) };
        break;
      case JOB.pollMatchesTick:
        result = { fannedOut: await fanOut(JOB.pollMatches) };
        break;
      case JOB.pollLive:
        result = await pollLive(job as Job<PollPlayerJob>);
        break;
      case JOB.pollRank:
        result = await pollRank(job as Job<PollPlayerJob>);
        break;
      case JOB.pollMatches:
        result = await pollMatches(job as Job<PollPlayerJob>);
        break;
      case JOB.archiveMatch:
        result = await archiveMatchJob(job as Job<ArchiveMatchJob>);
        break;
      case JOB.backfillPlayer:
        result = await backfillPlayer(job as Job<BackfillPlayerJob>);
        break;
      case JOB.ddragonSync:
        result = await ddragonSync(job as Job<{ force?: boolean }>);
        break;
      case JOB.maintenance:
        result = await maintenance();
        break;
      default:
        throw new Error(`Unknown job '${job.name}'`);
    }
    jobsTotal.inc({ job: job.name, status: 'completed' });
    return result;
  } catch (err) {
    jobsTotal.inc({ job: job.name, status: 'failed' });
    throw err;
  }
}

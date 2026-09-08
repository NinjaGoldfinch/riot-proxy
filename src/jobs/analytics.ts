import { setTimeout as sleep } from 'node:timers/promises';
import type { Job } from 'bullmq';
import { config } from '../config.js';
import {
  recomputeChampionBuilds,
  recomputeChampionMatchups,
  recomputeChampionStats,
} from '../db/analytics.js';
import { countArchivedMatches, reextractBatch } from '../db/matches.js';
import { logger } from '../logger.js';
import { assertRankedQueue } from '../riot/ladder.js';
import { assertPlatform } from '../riot/routing.js';
import { clearReextractCursor, getReextractCursor, setReextractCursor } from './facts-state.js';
import { JOB, jobKey, maintenanceQueue } from './queues.js';

/**
 * The jobs that read the archive back: `aggregate:champions`, which recomputes
 * every analytics table from it, and `facts:reextract`, which sweeps the
 * pre-C2 archive so those recomputes have facts to read.
 *
 * Split out of `processors.ts` for #122, ahead of #114 turning the one
 * recompute step below into five.
 */

/**
 * Queue a recompute for one ladder.
 *
 * Lifecycle-scoped de-duplication rather than a stable `jobId`: BullMQ matches
 * a job id against finished jobs it has retained, so a stable one would make
 * the *second* crawl of the day a silent no-op (#18). What this drops instead
 * is only a recompute that has not run yet — and one that has not run yet will
 * read the same tables the dropped one would have.
 */
export async function enqueueChampionAggregate(platform: string, queue: string): Promise<void> {
  await maintenanceQueue.add(
    JOB.aggregateChampions,
    { platform, queue } satisfies AggregateChampionsJob,
    { deduplication: { id: jobKey(JOB.aggregateChampions, platform, queue) } },
  );
}

export interface AggregateChampionsJob {
  platform: string;
  queue: string;
}

/**
 * Read the archive back into `champion_stats` and the second-order tables
 * (§7 of the plan, #112) — one job, several independently-transactional
 * recomputes, in this order.
 *
 * On the `maintenance` queue rather than `ladder`, because it is not part of
 * the crawl: it touches Riot not at all, and a crawl should be free to finish
 * — and free the ladder for the next one — without waiting on a table scan.
 * Nothing here is prioritized, matching the daily job beside it; giving one of
 * two jobs on a queue a priority would put the other permanently ahead of it.
 *
 * Each recompute commits its own tables in its own transaction (§9.1) rather
 * than one transaction spanning all of them: a crash mid-run leaves earlier
 * tables newer than later ones, which `computed_at` makes visible on every
 * row, and the next run converges — cheaper than one transaction holding
 * locks across a scan of the whole archive.
 */
export async function aggregateChampions(job: Job<AggregateChampionsJob>): Promise<{
  rows: number;
  games: number;
  matchups: number;
  items: number;
  runes: number;
  spells: number;
  ms: number;
}> {
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);

  const started = Date.now();
  const result = await recomputeChampionStats(platform, queue);
  logger.info(
    { platform, queue, rows: result.rows, games: result.games, ms: Date.now() - started },
    'champion aggregates recomputed',
  );

  const matchups = await recomputeChampionMatchups(platform, queue);
  logger.info({ platform, queue, rows: matchups.rows }, 'champion matchups recomputed');

  const builds = await recomputeChampionBuilds(platform, queue);
  logger.info(
    { platform, queue, items: builds.items, runes: builds.runes, spells: builds.spells },
    'champion builds recomputed',
  );

  // Every table's row count, not just `champion_stats`'. The admin recompute
  // route answers 202 and the job result is the only structured signal an
  // operator gets — a matchup or build step that inserted nothing (a pre-C2
  // archive with no `team_id` extracted, say) would otherwise show up only in
  // the logs, behind a `rows` count that looked healthy.
  return {
    rows: result.rows,
    games: result.games,
    matchups: matchups.rows,
    items: builds.items,
    runes: builds.runes,
    spells: builds.spells,
    ms: Date.now() - started,
  };
}

// ── facts:reextract ─────────────────────────────────────────────────────────

/** Queue a re-extraction of the whole archive's facts (§5.3 of the plan). */
export async function enqueueFactsReextract(): Promise<void> {
  await maintenanceQueue.add(
    JOB.factsReextract,
    {},
    // Lifecycle-scoped, like `enqueueChampionAggregate`: the walk has no id of
    // its own, and a second trigger while one is already in flight must join
    // it rather than start a competing walk over the same cursor.
    { deduplication: { id: jobKey(JOB.factsReextract) } },
  );
}

/** Milliseconds between batches — pure Postgres work at maintenance
 * concurrency 1, paced so it never competes with interactive traffic for the
 * connection pool (§5.3 of the plan). */
const REEXTRACT_PACE_MS = 50;

/**
 * Backfill the widened `match_participants` columns and `match_bans` for
 * every row the archive holds (#110) — everything `archiveMatch` wrote before
 * C2 existed has null fact columns and no bans.
 *
 * One job walks the whole archive rather than one batch per job: the cursor
 * lives in Redis (`facts-state.ts`) precisely so a crash or redeploy resumes
 * the walk instead of restarting it, and BullMQ's own lock renewal is what
 * lets a job run for as long as an archive this size takes.
 */
export async function factsReextract(job: Job): Promise<{ matches: number; batches: number }> {
  let cursor = await getReextractCursor();
  let matches = 0;
  let batches = 0;

  // Counted once, not per batch: a full-table count is itself an O(archive)
  // scan, and re-paying it every batch would compete with the very pacing
  // this job exists to respect. A denominator fixed at the walk's start is
  // still a true progress read — matches archived mid-walk simply count
  // toward the *next* run, the same way a crawl completing mid-walk does.
  const archived = await countArchivedMatches();

  for (;;) {
    const batch = await reextractBatch(cursor, config.FACTS_REEXTRACT_BATCH);
    if (batch.matchIds.length === 0) break;

    // Non-null: `reextractBatch` only returns a null cursor alongside an
    // empty `matchIds`, which the check above already ruled out.
    cursor = batch.cursor!;
    await setReextractCursor(cursor);
    matches += batch.matchIds.length;
    batches += 1;

    await job.updateProgress({ matches, cursor, archived });
    logger.info({ matches, batches, cursor, archived }, 'facts reextract progress');

    await sleep(REEXTRACT_PACE_MS);
  }

  // The whole archive is caught up, so there is nothing left to resume — the
  // next trigger (a later schema widening) should start from the beginning.
  await clearReextractCursor();
  logger.info({ matches, batches }, 'facts reextract complete');
  return { matches, batches };
}

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
import {
  aggregateRows,
  aggregateRunsTotal,
  aggregateStepDuration,
  factsReextractProgress,
} from '../metrics.js';
import { assertRankedQueue } from '../riot/ladder.js';
import { assertPlatform } from '../riot/routing.js';
import { recordAnalyticsRun } from './analytics-state.js';
import { clearReextractCursor, getReextractCursor, setReextractCursor } from './facts-state.js';
import { JOB, jobKey, maintenanceQueue } from './queues.js';

/**
 * The jobs that read the archive back: `aggregate:analytics`, which recomputes
 * every analytics table from it, and `facts:reextract`, which sweeps the
 * pre-C2 archive so those recomputes have facts to read.
 *
 * Both are bounded rather than unbounded — the recompute by
 * `AGGREGATE_PATCH_LIMIT`, the sweep by a resumable cursor — and both report
 * per-step timings and row counts, because a job that scans the archive is one
 * an operator has to be able to watch (#114).
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
export async function enqueueAnalyticsRecompute(platform: string, queue: string): Promise<void> {
  await maintenanceQueue.add(
    JOB.aggregateAnalytics,
    { platform, queue } satisfies AggregateAnalyticsJob,
    { deduplication: { id: jobKey(JOB.aggregateAnalytics, platform, queue) } },
  );
}

export interface AggregateAnalyticsJob {
  platform: string;
  queue: string;
}

/** Rows written per table by one run, for the job result and `proxy_aggregate_rows`. */
export interface AggregateAnalyticsResult {
  platform: string;
  queue: string;
  rows: Record<string, number>;
  games: number;
  ms: number;
}

/**
 * Read the archive back into every analytics table (§7 of the plan, #112,
 * #114) — one job, several independently-transactional recomputes, in this
 * order: champion (slices, `champion_stats`, bans) → matchups → builds.
 *
 * On the `maintenance` queue rather than `ladder`, because it is not part of
 * the crawl: it touches Riot not at all, and a crawl should be free to finish
 * — and free the ladder for the next one — without waiting on a table scan.
 * Nothing here is prioritized, matching the daily jobs beside it; giving one of
 * three jobs on a queue a priority would put the others permanently ahead of it.
 *
 * **Why three transactions and not five.** #114 asks for one per table. The
 * matchup and build steps get that, and it buys what the issue says it does: a
 * crash leaves them stale behind a fresh `champion_stats`, `computed_at` says
 * so on every row (and both routes now publish it), and the next run converges.
 *
 * `analytics_slices`, `champion_stats` and `champion_bans` stay in one
 * transaction, deliberately. They are not three sections a reader consults
 * separately — they are a numerator and its denominators. `pickRate` is
 * `champion_stats.matches_picked` over `analytics_slices.matches`, and
 * `banRate` is `champion_bans.bans` over the same slice. Committing them apart
 * means a crash can leave a rate computed against a denominator from a
 * different run, which does not read as stale — it reads as a plausible number
 * that is simply wrong. A stale section announces itself; a wrong rate does
 * not, and no `computed_at` can catch it.
 *
 * Each step is timed and counted separately regardless, which is what §13's
 * metrics were actually for: knowing *which* step got slow.
 */
export async function aggregateAnalytics(
  job: Job<AggregateAnalyticsJob>,
): Promise<AggregateAnalyticsResult> {
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);
  const labels = { platform, queue };

  const started = Date.now();
  const rows: Record<string, number> = {};
  const steps: Record<string, number> = {};
  let games: number;

  /** One step: timed, counted, and its row counts published per table. */
  const step = async <T>(
    name: string,
    run: () => Promise<T>,
    counts: (result: T) => Record<string, number>,
  ): Promise<T> => {
    const at = Date.now();
    const result = await run();
    const seconds = (Date.now() - at) / 1000;
    aggregateStepDuration.observe({ ...labels, step: name }, seconds);
    steps[name] = seconds;
    for (const [table, n] of Object.entries(counts(result))) {
      rows[table] = n;
      aggregateRows.set({ ...labels, table }, n);
    }
    logger.info({ ...labels, step: name, seconds, ...counts(result) }, 'recompute step done');
    return result;
  };

  try {
    games = (
      await step(
        'champions',
        () => recomputeChampionStats(platform, queue),
        (r) => ({ champion_stats: r.rows }),
      )
    ).games;

    await step(
      'matchups',
      () => recomputeChampionMatchups(platform, queue),
      (r) => ({ champion_matchups: r.rows }),
    );

    await step(
      'builds',
      () => recomputeChampionBuilds(platform, queue),
      (r) => ({ champion_items: r.items, champion_runes: r.runes, champion_spells: r.spells }),
    );
  } catch (err) {
    // Counted *and recorded* before rethrowing, because a run that died half
    // way is exactly the state the per-step transactions permit and the one
    // worth seeing — the steps that did finish are in `steps`, which is what
    // says how far it got.
    aggregateRunsTotal.inc({ ...labels, status: 'failed' });
    await recordAnalyticsRun({
      ...labels,
      at: Date.now(),
      status: 'failed',
      ms: Date.now() - started,
      steps,
      rows,
      games: 0,
    });
    throw err;
  }

  aggregateRunsTotal.inc({ ...labels, status: 'completed' });
  const ms = Date.now() - started;
  await recordAnalyticsRun({
    ...labels,
    at: Date.now(),
    status: 'completed',
    ms,
    steps,
    rows,
    games,
  });
  logger.info({ ...labels, rows, steps, games, ms }, 'analytics recomputed');

  // Every table's row count, not just `champion_stats`'. The admin recompute
  // route answers 202 and the job result is the only structured signal an
  // operator gets — a matchup or build step that inserted nothing (a pre-C2
  // archive with no `team_id` extracted, say) would otherwise show up only in
  // the logs, behind a `rows` count that looked healthy.
  return { platform, queue, rows, games, ms };
}

// ── facts:reextract ─────────────────────────────────────────────────────────

/** Queue a re-extraction of the whole archive's facts (§5.3 of the plan). */
export async function enqueueFactsReextract(): Promise<void> {
  await maintenanceQueue.add(
    JOB.factsReextract,
    {},
    // Lifecycle-scoped, like `enqueueAnalyticsRecompute`: the walk has no id of
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

    // The same number `job.updateProgress` carries, published where an
    // operator is already looking: the sweep is the slowest job here and the
    // only one whose progress nothing else reveals — while it runs the
    // aggregates it feeds look thin rather than incomplete.
    if (archived > 0) factsReextractProgress.set(Math.min(1, matches / archived));
    await job.updateProgress({ matches, cursor, archived });
    logger.info({ matches, batches, cursor, archived }, 'facts reextract progress');

    await sleep(REEXTRACT_PACE_MS);
  }

  // The whole archive is caught up, so there is nothing left to resume — the
  // next trigger (a later schema widening) should start from the beginning.
  await clearReextractCursor();
  // Caught up, whether this run swept the whole archive or found it already
  // swept — an idle `0` would read as "never started" rather than "done".
  factsReextractProgress.set(1);
  logger.info({ matches, batches }, 'facts reextract complete');
  return { matches, batches };
}

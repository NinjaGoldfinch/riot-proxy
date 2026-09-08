import { backfillNamesFromArchive } from '../db/players.js';
import { logger } from '../logger.js';
import { JOB, jobKey, maintenanceQueue } from './queues.js';

/**
 * `names:backfill` (#120) — Riot IDs for the PUUIDs a crawl discovered, read
 * back out of matches the archive already holds.
 *
 * Its own file since #122. It sits on the `maintenance` queue beside
 * `aggregate:champions` for the same reason that one does — it reads tables
 * and touches Riot not at all — but "what a player is called" is not an
 * analytics question, and the two share no code.
 */

/**
 * Put Riot IDs on the PUUIDs a crawl discovered, out of the archive.
 *
 * On the `maintenance` queue beside `aggregate:champions` and for the same
 * reason: it reads tables and touches Riot not at all, so it has no business
 * holding up the ladder queue — and a crawl should be free to finish, and free
 * that ladder for the next run, without waiting on a scan.
 *
 * Unprioritized, matching the two jobs already there. Giving one job on a
 * queue a priority puts every unprioritized one permanently ahead of it, which
 * is the opposite of what a priority looks like it does.
 */
export async function backfillNames(): Promise<{ named: number; unnamed: number }> {
  const started = Date.now();
  const result = await backfillNamesFromArchive();
  logger.info(
    { named: result.named, unnamed: result.unnamed, ms: Date.now() - started },
    'player names backfilled from archive',
  );
  return result;
}

/**
 * Queue one pass. Lifecycle-scoped de-duplication for the same reason as
 * `enqueueChampionAggregate`: a stable `jobId` is matched against retained
 * finished jobs too, so it would silence every pass after the first for the
 * length of the retention window (#18). Dropping a pass that has not run yet
 * costs nothing — the one that replaces it reads the same tables.
 */
export async function enqueueNameBackfill(): Promise<void> {
  await maintenanceQueue.add(
    JOB.namesBackfill,
    {},
    { deduplication: { id: jobKey(JOB.namesBackfill) } },
  );
}

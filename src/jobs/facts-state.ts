import { KEY_SCOPE } from '../config.js';
import { redis } from '../redis.js';
import { CURSOR_TTL_S } from './ladder-state.js';

/**
 * `facts:reextract`'s progress (#110) — the one piece of state a resumable,
 * whole-archive walk needs: where the last committed batch left off.
 *
 * One cursor per key scope, not per run: unlike a crawl, a reextract has no
 * id of its own — it is "catch the archive up to the current extraction
 * logic" — and a second trigger while one is already in flight is refused by
 * the job's own lifecycle-scoped dedupe id rather than starting a second walk.
 * Reusing `CURSOR_TTL_S` is deliberate — the same trade-off applies verbatim:
 * long enough that a job paused by a redeploy still finds its place, short
 * enough that an abandoned run does not pin a stale cursor forever.
 */
const cursorKey = (): string => `facts:reextract:cursor:${KEY_SCOPE}`;

/**
 * The `match_id` to resume after. Null both when nothing has ever run and
 * when the last run reached the end of the archive and cleared its own
 * cursor — either way, the next run starts from the beginning.
 */
export async function getReextractCursor(): Promise<string | null> {
  return redis.get(cursorKey());
}

/**
 * Written after a batch's upserts commit, so a crash between the two re-walks
 * one batch — cheap and idempotent — rather than silently skipping the
 * matches inside it.
 */
export async function setReextractCursor(matchId: string): Promise<void> {
  await redis.set(cursorKey(), matchId, 'EX', CURSOR_TTL_S);
}

/** The walk reached the end of the archive: nothing left to resume. */
export async function clearReextractCursor(): Promise<void> {
  await redis.del(cursorKey());
}

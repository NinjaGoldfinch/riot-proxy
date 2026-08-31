import { KEY_SCOPE } from '../config.js';
import { redis } from '../redis.js';
import type { Division, PagedTier } from '../riot/ladder.js';

/**
 * A crawl's working state, the parts too churny for Postgres: where each walk
 * has got to, and which of its legs are still outstanding.
 *
 * Where a (tier, division) walk has got to (§5 of docs/ladder-crawl-plan.md).
 *
 * These live in Redis rather than on the `ladder_crawls` row because they
 * churn on every page — a full-ladder crawl advances a cursor ~20 000 times,
 * and none of those writes is worth a durable row. What they buy is the
 * difference between a crashed walk resuming from page 400 and restarting from
 * page 1; losing one to a Redis flush costs a re-walk, not correctness, since
 * the upserts are idempotent.
 *
 * Keyed by crawl id, so a cursor stranded by a crash belongs to a run nothing
 * will ever resume and expires on its own.
 */

const cursorKey = (crawlId: string, tier: PagedTier, division: Division): string =>
  `ladder:cursor:${KEY_SCOPE}:${crawlId}:${tier}:${division}`;

/**
 * Long enough that a crawl paused by a limiter freeze still finds its place,
 * short enough that abandoned runs do not accumulate. A full dev-key ladder
 * enumeration is ~5–7 hours (§2), so a day is roughly four times the slowest
 * crawl the design contemplates.
 */
export const CURSOR_TTL_S = 24 * 3600;

/** The next page to fetch. 1 — the first page — when nothing is recorded. */
export async function getCursor(
  crawlId: string,
  tier: PagedTier,
  division: Division,
): Promise<number> {
  const value = await redis.get(cursorKey(crawlId, tier, division));
  const page = Number(value);
  return Number.isInteger(page) && page > 0 ? page : 1;
}

/**
 * Record where to resume. Written *after* the page's entries are upserted, so
 * a crash between the two re-walks one page rather than skipping it — the
 * upserts are idempotent and a duplicate page costs one request, while a
 * skipped page silently loses 205 players from the ladder.
 */
export async function setCursor(
  crawlId: string,
  tier: PagedTier,
  division: Division,
  nextPage: number,
): Promise<void> {
  await redis.set(cursorKey(crawlId, tier, division), String(nextPage), 'EX', CURSOR_TTL_S);
}

/**
 * Drop a finished crawl's cursors. They would expire regardless; clearing them
 * on completion keeps a long-lived Redis from carrying a day of dead keys per
 * crawl, and makes "cursors present" a usable signal for a crawl still moving.
 */
export async function clearCursors(crawlId: string): Promise<number> {
  const pattern = `ladder:cursor:${KEY_SCOPE}:${crawlId}:*`;
  let cursor = '0';
  let deleted = 0;

  do {
    // SCAN rather than KEYS: a full-ladder crawl has 28 cursors, but this runs
    // on the same Redis the limiter and cache use, and KEYS blocks all of it.
    const [next, keys] = await redis.scan(cursor, 'MATCH', pattern, 'COUNT', 200);
    cursor = next;
    if (keys.length > 0) deleted += await redis.del(...keys);
  } while (cursor !== '0');

  return deleted;
}

// ── outstanding legs ─────────────────────────────────────────────────────────

/**
 * A crawl is finished when its last leg is — an apex league or a
 * (tier, division) walk. Which is easy to say and awkward to observe: BullMQ
 * cannot be asked "any jobs left for crawl X", and a counter on the crawl row
 * would be written by every leg on every attempt.
 *
 * So the fan-out records the legs it created, and each one removes itself as
 * it ends. Whoever removes the last member finishes the crawl. A set rather
 * than a counter because `SREM` is idempotent: a leg that retries after
 * already removing itself takes nothing away twice, which a `DECR` would.
 */
const legsKey = (crawlId: string): string => `ladder:legs:${KEY_SCOPE}:${crawlId}`;

/** Set when a leg gives up for good, so the crawl ends `failed`, not `completed`. */
const failedKey = (crawlId: string): string => `ladder:failed:${KEY_SCOPE}:${crawlId}`;

/** Record the legs a fan-out just created. */
export async function trackLegs(crawlId: string, legIds: string[]): Promise<void> {
  if (legIds.length === 0) return;
  await redis
    .multi()
    .sadd(legsKey(crawlId), ...legIds)
    .expire(legsKey(crawlId), CURSOR_TTL_S)
    .exec();
}

/**
 * A leg has ended. Returns whether it was the last one — and therefore whether
 * this caller is the one that should finish the crawl.
 *
 * Two legs ending at once can both read an empty set, so both would try. That
 * is fine and deliberately not locked against: `finishCrawl` is guarded on
 * `status = 'running'`, so the second one changes nothing.
 */
export async function releaseLeg(
  crawlId: string,
  legId: string,
  outcome: 'done' | 'failed' = 'done',
): Promise<{ last: boolean; failed: boolean }> {
  if (outcome === 'failed') {
    await redis.set(failedKey(crawlId), '1', 'EX', CURSOR_TTL_S);
  }
  await redis.srem(legsKey(crawlId), legId);
  const remaining = await redis.scard(legsKey(crawlId));
  if (remaining > 0) return { last: false, failed: false };

  const failed = (await redis.exists(failedKey(crawlId))) === 1;
  return { last: true, failed };
}

/** How much of a crawl is still outstanding, for the admin listing. */
export async function pendingLegs(crawlId: string): Promise<number> {
  return redis.scard(legsKey(crawlId));
}

/** Everything a finished or cancelled crawl leaves behind. */
export async function clearCrawlState(crawlId: string): Promise<void> {
  await Promise.all([clearCursors(crawlId), redis.del(legsKey(crawlId), failedKey(crawlId))]);
}

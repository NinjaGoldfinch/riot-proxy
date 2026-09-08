import { config } from '../config.js';
import { fetcher, type FetchOptions } from '../fetcher.js';
import { build } from '../riot/endpoints.js';
import type { Region } from '../riot/routing.js';

/**
 * Paging one player's match ids, and deciding what a walk earned (#122).
 *
 * Lifted out of `processors.ts` because both of its callers are now in
 * different files — the lookup backfill in `processors.ts`, a crawl's collect
 * stage in `ladder-crawl.ts` — and a helper that two job families share is the
 * one thing neither of them should own.
 */

const BACKFILL_PAGE = 100;

/**
 * One player's match ids, newest first, a page at a time.
 *
 * The paging is the same wherever it is done — the lookup backfill queues each
 * page's unarchived matches as it goes, and a crawl's collect stage puts them
 * in a set instead — so the loop lives here and the caller says what a page is
 * for. `start` comes with the page because both callers rank by position in
 * the *history*: skipping ten already-archived matches must not promote the
 * eleventh.
 *
 * `ranOut` distinguishes a history that ended from a walk that hit its limit,
 * which is what decides whether the player can be stamped as backfilled.
 */
export async function walkMatchIds(
  region: Region,
  puuid: string,
  /**
   * `fetch` is required rather than defaulted: the two callers want different
   * wait budgets — a backfill has somewhere else to be, a crawl does not — and
   * a default here would quietly give one of them the other's.
   */
  options: { limit: number; queueId?: number; fetch: FetchOptions },
  onPage: (ids: string[], start: number) => Promise<void>,
): Promise<{ depth: number; ranOut: boolean }> {
  const { limit, queueId } = options;
  let start = 0;
  let depth = 0;

  while (start < limit) {
    const count = Math.min(BACKFILL_PAGE, limit - start);
    const { data: ids } = await fetcher.fetch<string[]>(
      build.matchIdsByPuuid(region, puuid, {
        start,
        count,
        ...(queueId !== undefined ? { queue: queueId } : {}),
      }),
      options.fetch,
    );
    if (!ids || ids.length === 0) break;

    await onPage(ids, start);
    depth = start + ids.length;

    // Reached the end of this player's history.
    if (ids.length < count) return { depth, ranOut: true };
    start += ids.length;
  }

  return { depth, ranOut: false };
}

/**
 * Whether a walk that got this far has earned the "someone has this player's
 * history" stamp.
 *
 * A *shallow* walk has not. `historyBackfilledAt` means the whole history is
 * accounted for, and a walk that stopped at its own limit has only the top of
 * it — so a 100-match ladder walk must not lock a player out of the
 * 10 000-match walk their first lookup would have done. Two ways to have
 * earned it: the history ran out before the limit did, or the limit was at
 * least as deep as the lookup path asks for.
 *
 * `ranOut` only counts for an unfiltered walk: running out of *ranked* ids
 * says nothing about the rest of a player's history.
 */
export function walkIsComplete(ranOut: boolean, limit: number, queueId?: number): boolean {
  return (ranOut && queueId === undefined) || limit >= config.LOOKUP_BACKFILL_LIMIT;
}

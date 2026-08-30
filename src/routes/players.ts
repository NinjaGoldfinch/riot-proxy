import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync, FastifyReply } from 'fastify';
import { config } from '../config.js';
import { upsertPlayer } from '../db/players.js';
import { ProxyError } from '../errors.js';
import { fetcher, type FetchResult } from '../fetcher.js';
import { enqueueBackfill } from '../jobs/queues.js';
import { logger } from '../logger.js';
import { redis } from '../redis.js';
import { build } from '../riot/endpoints.js';
import {
  assertPlatform,
  platformToAccountRegion,
  platformToRegion,
  type Platform,
} from '../riot/routing.js';
import { applyCacheHeaders } from './helpers.js';
import { summariseMatch, type MatchSummary } from './match-summary.js';
import {
  GameNameParam,
  MatchPageQuery,
  MatchPageResponse,
  PassthroughResponse,
  PlatformParam,
  PuuidParam,
  TagLineParam,
  errorResponses,
} from './schemas.js';

/**
 * §6.3 — the composite endpoints, the proxy's biggest ergonomic win: one client
 * call fans out to several Riot calls server-side, each individually cached.
 *
 * Partial failure returns the parts that succeeded plus `warnings[]` — a
 * mastery timeout must never fail the whole document.
 *
 * Reads are cache-first: viewing a profile costs nothing upstream while the
 * cache holds it, and only an outright miss goes to Riot. Spending quota to
 * refresh a player who has not played since the last look is the caller's
 * decision to make, so it takes an explicit `?refresh=true` — metered by
 * `refreshWindow` below.
 */

/**
 * §12.1 in spirit: `refresh` is the one thing a consumer can ask for that is
 * guaranteed to cost upstream quota, so it is metered per player here rather
 * than trusted to whatever UI is driving it.
 */
export const REFRESH_COOLDOWN_S = 60;

interface RefreshState {
  /** This request won the window and went upstream. */
  refreshed: boolean;
  /** Seconds until another manual refresh is allowed; 0 when allowed now. */
  availableIn: number;
}

/**
 * Claim the refresh window for a player, or report how long is left on it.
 *
 * Losing the race is not an error: whoever won it wrote fresh values less than
 * a minute ago, so the cache read the caller falls back to is the very data
 * they asked to be fetched.
 */
async function refreshWindow(
  part: string,
  identity: string,
  claim: boolean,
): Promise<RefreshState> {
  const key = `refresh:${config.KEY_SCOPE}:${part}:${identity}`;
  if (claim && (await redis.set(key, '1', 'EX', REFRESH_COOLDOWN_S, 'NX'))) {
    return { refreshed: true, availableIn: REFRESH_COOLDOWN_S };
  }
  const ttl = await redis.ttl(key);
  return { refreshed: false, availableIn: ttl > 0 ? ttl : 0 };
}

const RefreshQuery = Type.Optional(Type.Boolean({ default: false }));
const playerRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/players/:puuid/profile',
    {
      schema: {
        tags: ['players'],
        params: Type.Object({ puuid: PuuidParam }),
        querystring: Type.Object({
          platform: Type.Optional(PlatformParam),
          topMastery: Type.Optional(Type.Integer({ minimum: 1, maximum: 20, default: 5 })),
          refresh: RefreshQuery,
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { puuid } = request.params as { puuid: string };
      const {
        platform: platformRaw,
        topMastery = 5,
        refresh = false,
      } = request.query as { platform?: string; topMastery?: number; refresh?: boolean };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      const window = await refreshWindow('profile', puuid, refresh);
      return finish(reply, await composeProfile({ platform, puuid, topMastery, window }));
    },
  );

  /**
   * The same document, entered by Riot ID. A browser only ever has the name a
   * player types, and resolving the PUUID client-side means a round trip whose
   * only purpose is to feed the next one — so the account lookup happens here
   * and its result is reused as the composite's `account` part rather than
   * being fetched a second time.
   */
  fastify.get(
    '/v1/players/by-riot-id/:gameName/:tagLine/profile',
    {
      schema: {
        tags: ['players'],
        params: Type.Object({ gameName: GameNameParam, tagLine: TagLineParam }),
        querystring: Type.Object({
          platform: Type.Optional(PlatformParam),
          topMastery: Type.Optional(Type.Integer({ minimum: 1, maximum: 20, default: 5 })),
          refresh: RefreshQuery,
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { gameName, tagLine } = request.params as { gameName: string; tagLine: string };
      const {
        platform: platformRaw,
        topMastery = 5,
        refresh = false,
      } = request.query as { platform?: string; topMastery?: number; refresh?: boolean };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      // Always the cached mapping, even on a refresh: it is only here to turn
      // the Riot ID into a PUUID, which is what the cooldown is keyed on. A
      // refresh then re-fetches the account by PUUID along with the rest.
      const account = await fetcher.fetch<{ puuid?: string }>(
        build.accountByRiotId(platformToAccountRegion(platform), gameName, tagLine),
      );

      const puuid = account.data?.puuid;
      if (!puuid) {
        throw ProxyError.notFound(`Riot ID '${gameName}#${tagLine}' did not resolve to a PUUID`);
      }

      const window = await refreshWindow('profile', puuid, refresh);
      return finish(
        reply,
        await composeProfile({
          platform,
          puuid,
          topMastery,
          window,
          ...(window.refreshed ? {} : { account }),
        }),
      );
    },
  );

  /**
   * Match history in one call: the id page, then every match on it, fanned out
   * concurrently. Matches are immutable, so a second page view is served from
   * the Postgres archive at zero upstream cost (§7.3) — which is what makes
   * paging through a history affordable at all.
   *
   * What comes back is a summary per match, not the match: an overview panel
   * needs a champion, a scoreline and six items, and a page of ten full
   * payloads is about a megabyte of response to carry that. The full document
   * stays one call away at `/v1/lol/matches/{region}/{matchId}`, served from
   * the archive — see `match-summary.ts`.
   */
  fastify.get(
    '/v1/players/:puuid/matches',
    {
      schema: {
        tags: ['players'],
        params: Type.Object({ puuid: PuuidParam }),
        querystring: MatchPageQuery,
        response: { 200: MatchPageResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { puuid } = request.params as { puuid: string };
      const {
        platform: platformRaw,
        start = 0,
        count = 10,
        queue,
        type,
        refresh = false,
      } = request.query as {
        platform?: string;
        start?: number;
        count?: number;
        queue?: number;
        type?: string;
        refresh?: boolean;
      };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      const window = await refreshWindow('matches', puuid, refresh);
      return finish(
        reply,
        await composeMatches({ platform, puuid, start, count, queue, type, window }),
      );
    },
  );
};

interface Composed<T> {
  body: T;
  cache: 'HIT' | 'MISS';
  ageSeconds: number;
}

/** Apply the composite's cache headers and hand the body back to Fastify. */
function finish<T>(reply: FastifyReply, composed: Composed<T>): T {
  applyCacheHeaders(reply, composed.cache, composed.ageSeconds);
  return composed.body;
}

/**
 * A composite is as fresh as its stalest successful part, and counts as a MISS
 * if any part had to go upstream.
 */
function summarise(results: FetchResult<unknown>[]): { cache: 'HIT' | 'MISS'; ageSeconds: number } {
  const ageSeconds = Math.max(0, ...results.map((r) => r.ageSeconds));
  return { cache: results.some((r) => r.cache === 'MISS') ? 'MISS' : 'HIT', ageSeconds };
}

/** Collects the failures of a fan-out into `warnings[]` without failing it. */
function collector(warnings: string[]) {
  return <T>(name: string, settled: PromiseSettledResult<FetchResult<T>>): T | null => {
    if (settled.status === 'fulfilled') return settled.value.data;
    const reason = settled.reason;
    const message =
      reason instanceof ProxyError ? `${reason.code}: ${reason.message}` : 'unavailable';
    warnings.push(`${name} unavailable (${message})`);
    logger.debug({ part: name, err: reason }, 'composite part failed');
    return null;
  };
}

function fulfilled(settled: PromiseSettledResult<FetchResult<unknown>>[]): FetchResult<unknown>[] {
  return settled
    .filter((s): s is PromiseFulfilledResult<FetchResult<unknown>> => s.status === 'fulfilled')
    .map((s) => s.value);
}

interface ProfileBody {
  puuid: string;
  platform: Platform;
  region: string;
  account: unknown;
  summoner: unknown;
  league: unknown;
  mastery: unknown;
  /**
   * Per part, how long its content has been unchanged. The top-level
   * `X-Cache-Age` is the stalest of these, which is the right answer for a
   * cache header and the wrong one for a caller labelling four independent
   * sections — an account that has not changed in a day would otherwise make a
   * rank that moved a minute ago look a day old.
   */
  ageSeconds: Record<string, number | null>;
  refreshed: boolean;
  refreshAvailableIn: number;
  warnings: string[];
}

async function composeProfile(opts: {
  platform: Platform;
  puuid: string;
  topMastery: number;
  window: RefreshState;
  /** Already-fetched account part, when the caller entered by Riot ID. */
  account?: FetchResult<unknown>;
}): Promise<Composed<ProfileBody>> {
  const { platform, puuid, topMastery, window } = opts;
  // A won refresh bypasses the read on every part; the writes still happen, so
  // the next plain lookup serves what this one fetched.
  const fresh = { bypassCache: window.refreshed };
  // `region` is echoed to the caller as the match-v5 host for their platform;
  // account-v1 needs its own, which for SEA is not the same.
  const region = platformToRegion(platform);

  // Fan out concurrently; per-part caching still applies (§6.3).
  const settled = await Promise.allSettled([
    opts.account
      ? Promise.resolve(opts.account)
      : fetcher.fetch(build.accountByPuuid(platformToAccountRegion(platform), puuid), fresh),
    fetcher.fetch(build.summonerByPuuid(platform, puuid), fresh),
    fetcher.fetch(build.leagueEntriesByPuuid(platform, puuid), fresh),
    fetcher.fetch(build.masteryTopByPuuid(platform, puuid, topMastery), fresh),
  ]);

  const warnings: string[] = [];
  const part = collector(warnings);
  const [account, summoner, league, mastery] = settled;
  const age = (s: PromiseSettledResult<FetchResult<unknown>>) =>
    s.status === 'fulfilled' ? s.value.ageSeconds : null;

  const body: ProfileBody = {
    puuid,
    platform,
    region,
    account: part('account', account),
    summoner: part('summoner', summoner),
    league: part('league', league),
    mastery: part('mastery', mastery),
    ageSeconds: {
      account: age(account),
      summoner: age(summoner),
      league: age(league),
      mastery: age(mastery),
    },
    refreshed: window.refreshed,
    refreshAvailableIn: window.availableIn,
    warnings,
  };

  // Every part failing means the player does not resolve at all — that is a
  // 404, not a 200 full of nulls.
  if (!body.account && !body.summoner && !body.league && !body.mastery) {
    throw ProxyError.notFound('No profile data available for this PUUID');
  }

  await rememberIdentity(puuid, platform, body.account);

  return { body, ...summarise(fulfilled(settled)) };
}

/**
 * Keep what the account part told us about who this PUUID is. The row is where
 * backfill state lives (#44), and this is the one path that sees a Riot ID
 * without an admin typing it, so a player looked up by name arrives already
 * named rather than as a bare PUUID.
 */
async function rememberIdentity(
  puuid: string,
  platform: Platform,
  account: unknown,
): Promise<void> {
  const identity = account as { gameName?: unknown; tagLine?: unknown } | null;
  const gameName = typeof identity?.gameName === 'string' ? identity.gameName : undefined;
  const tagLine = typeof identity?.tagLine === 'string' ? identity.tagLine : undefined;

  try {
    await upsertPlayer({
      puuid,
      platform,
      ...(gameName !== undefined ? { gameName } : {}),
      ...(tagLine !== undefined ? { tagLine } : {}),
    });
  } catch (err) {
    // Bookkeeping. A profile is still a profile without it.
    logger.warn({ err, puuid }, 'could not record player identity');
  }
}

interface BackfillNotice {
  jobId: string;
  status: 'queued' | 'already-queued';
  limit: number;
}

/**
 * The first time anyone asks for a player, walk their whole history into the
 * archive rather than only the page that was asked for. Matches are immutable,
 * so every one stored now is one nobody ever spends quota on again — and the
 * work runs at `bulk` priority behind `BULK_USAGE_CEILING`, so it cannot eat
 * the headroom this very request came out of.
 *
 * "First time" is a fact about the player, kept on the player (#44). It used to
 * be inferred from the archive — no stored matches on the newest page meant new
 * — but matches are shared between ten players, so a single game archived
 * because a teammate was walked was enough to classify someone as already
 * known and skip them permanently. The more the archive filled, the more people
 * it silently locked out.
 *
 * So the ids are not consulted at all. A completed walk is the only thing that
 * stops another one; a walk that is merely in flight is stopped a layer down by
 * `enqueueBackfill`, which means a walk that died mid-way is retried here
 * rather than mistaken for a finished one.
 */
async function maybeBackfill(opts: {
  puuid: string;
  platform: Platform;
  start: number;
}): Promise<BackfillNotice | null> {
  const limit = config.LOOKUP_BACKFILL_LIMIT;
  // A deep page is someone paging through a history whose first page already
  // had its chance to trigger this.
  if (limit <= 0 || opts.start !== 0) return null;

  try {
    // Upserting is what creates the row on the lookup path — until now only an
    // admin track wrote one — and it returns the state, so asking whether they
    // are new and recording that we saw them is a single round trip.
    const player = await upsertPlayer({ puuid: opts.puuid, platform: opts.platform });
    if (player.historyBackfilledAt) return null;

    const { jobId, status } = await enqueueBackfill({
      puuid: opts.puuid,
      platform: opts.platform,
      limit,
      reason: 'lookup',
    });
    logger.info({ puuid: opts.puuid, jobId, status, limit }, 'queued backfill on first lookup');
    return { jobId, status, limit };
  } catch (err) {
    // Archiving is an optimisation. A queue that is down must not take the
    // match history down with it.
    logger.warn({ err, puuid: opts.puuid }, 'could not queue lookup backfill');
    return null;
  }
}

interface MatchPageBody {
  puuid: string;
  platform: Platform;
  region: string;
  start: number;
  count: number;
  matchIds: string[];
  /**
   * One summary per id that resolved — the requesting player's line in each
   * game, not the game (`match-summary.ts`). Shorter than `matchIds` when a
   * match could not be fetched; `warnings[]` names the ones missing.
   */
  matches: MatchSummary[];
  /** A full page came back, so there is probably another one behind it. */
  hasMore: boolean;
  /**
   * How long the id list has been unchanged. The matches behind it are
   * immutable, so this is the only age on the page that can mean anything.
   */
  matchIdsAgeSeconds: number;
  /** Set when this lookup queued the player's history for archiving. */
  backfill: BackfillNotice | null;
  refreshed: boolean;
  refreshAvailableIn: number;
  warnings: string[];
}

async function composeMatches(opts: {
  platform: Platform;
  puuid: string;
  start: number;
  count: number;
  queue?: number;
  type?: string;
  window: RefreshState;
}): Promise<Composed<MatchPageBody>> {
  const { platform, puuid, start, count, queue, type, window } = opts;
  const region = platformToRegion(platform);

  /**
   * The id page is the one part that cannot fail softly: no ids, no matches —
   * and the only part a refresh touches. Matches themselves are immutable and
   * archived (§7.3), so bypassing their cache would re-download games that
   * cannot have changed.
   */
  const ids = await fetcher.fetch<string[]>(
    build.matchIdsByPuuid(region, puuid, { start, count, queue, type }),
    { bypassCache: window.refreshed },
  );
  const matchIds = Array.isArray(ids.data) ? ids.data : [];

  const backfill = await maybeBackfill({ puuid, platform, start });

  const settled = await Promise.allSettled(
    matchIds.map((id) => fetcher.fetch(build.matchById(region, id))),
  );

  const warnings: string[] = [];
  const part = collector(warnings);
  // Failed matches are dropped rather than left as holes; `warnings[]` names
  // each one, so a caller that cares can still tell what is missing.
  const matches: MatchSummary[] = [];
  matchIds.forEach((id, index) => {
    const result = settled[index];
    if (!result) return;

    const match = part(`match ${id}`, result);
    if (match === null) return;

    // Fetched but unsummarisable means the payload holds no participant for
    // this PUUID — an archive row under the wrong id, or a malformed match.
    // Same treatment as a failed fetch: named, and left off the page.
    const summary = summariseMatch(match, puuid, id);
    if (summary === null) {
      warnings.push(`match ${id} unavailable (no participant for this player)`);
      logger.debug({ matchId: id, puuid }, 'match holds no participant for the requested player');
      return;
    }
    matches.push(summary);
  });

  const body: MatchPageBody = {
    puuid,
    platform,
    region,
    start,
    count,
    matchIds,
    matches,
    hasMore: matchIds.length === count,
    matchIdsAgeSeconds: ids.ageSeconds,
    backfill,
    refreshed: window.refreshed,
    refreshAvailableIn: window.availableIn,
    warnings,
  };

  return { body, ...summarise([ids, ...fulfilled(settled)]) };
}

export default playerRoutes;

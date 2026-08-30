import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync, FastifyReply } from 'fastify';
import { config } from '../config.js';
import { ProxyError } from '../errors.js';
import { fetcher, type FetchResult } from '../fetcher.js';
import { logger } from '../logger.js';
import { build } from '../riot/endpoints.js';
import {
  assertPlatform,
  platformToAccountRegion,
  platformToRegion,
  type Platform,
} from '../riot/routing.js';
import { applyCacheHeaders } from './helpers.js';
import {
  GameNameParam,
  MatchPageQuery,
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
 */
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
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { puuid } = request.params as { puuid: string };
      const { platform: platformRaw, topMastery = 5 } = request.query as {
        platform?: string;
        topMastery?: number;
      };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      return finish(reply, await composeProfile({ platform, puuid, topMastery }));
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
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { gameName, tagLine } = request.params as { gameName: string; tagLine: string };
      const { platform: platformRaw, topMastery = 5 } = request.query as {
        platform?: string;
        topMastery?: number;
      };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      // A miss here is a 404 for the whole request: without a PUUID there is
      // nothing left to fan out to.
      const account = await fetcher.fetch<{ puuid?: string }>(
        build.accountByRiotId(platformToAccountRegion(platform), gameName, tagLine),
      );

      const puuid = account.data?.puuid;
      if (!puuid) {
        throw ProxyError.notFound(`Riot ID '${gameName}#${tagLine}' did not resolve to a PUUID`);
      }

      return finish(reply, await composeProfile({ platform, puuid, topMastery, account }));
    },
  );

  /**
   * Match history in one call: the id page, then every match on it, fanned out
   * concurrently. Matches are immutable, so a second page view is served from
   * the Postgres archive at zero upstream cost (§7.3) — which is what makes
   * paging through a history affordable at all.
   */
  fastify.get(
    '/v1/players/:puuid/matches',
    {
      schema: {
        tags: ['players'],
        params: Type.Object({ puuid: PuuidParam }),
        querystring: MatchPageQuery,
        response: { 200: PassthroughResponse, ...errorResponses },
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
      } = request.query as {
        platform?: string;
        start?: number;
        count?: number;
        queue?: number;
        type?: string;
      };

      const platform = assertPlatform(platformRaw ?? config.DEFAULT_PLATFORM);
      return finish(reply, await composeMatches({ platform, puuid, start, count, queue, type }));
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
  warnings: string[];
}

async function composeProfile(opts: {
  platform: Platform;
  puuid: string;
  topMastery: number;
  /** Already-fetched account part, when the caller entered by Riot ID. */
  account?: FetchResult<unknown>;
}): Promise<Composed<ProfileBody>> {
  const { platform, puuid, topMastery } = opts;
  // `region` is echoed to the caller as the match-v5 host for their platform;
  // account-v1 needs its own, which for SEA is not the same.
  const region = platformToRegion(platform);

  // Fan out concurrently; per-part caching still applies (§6.3).
  const settled = await Promise.allSettled([
    opts.account
      ? Promise.resolve(opts.account)
      : fetcher.fetch(build.accountByPuuid(platformToAccountRegion(platform), puuid)),
    fetcher.fetch(build.summonerByPuuid(platform, puuid)),
    fetcher.fetch(build.leagueEntriesByPuuid(platform, puuid)),
    fetcher.fetch(build.masteryTopByPuuid(platform, puuid, topMastery)),
  ]);

  const warnings: string[] = [];
  const part = collector(warnings);
  const [account, summoner, league, mastery] = settled;

  const body: ProfileBody = {
    puuid,
    platform,
    region,
    account: part('account', account),
    summoner: part('summoner', summoner),
    league: part('league', league),
    mastery: part('mastery', mastery),
    warnings,
  };

  // Every part failing means the player does not resolve at all — that is a
  // 404, not a 200 full of nulls.
  if (!body.account && !body.summoner && !body.league && !body.mastery) {
    throw ProxyError.notFound('No profile data available for this PUUID');
  }

  return { body, ...summarise(fulfilled(settled)) };
}

interface MatchPageBody {
  puuid: string;
  platform: Platform;
  region: string;
  start: number;
  count: number;
  matchIds: string[];
  matches: unknown[];
  /** A full page came back, so there is probably another one behind it. */
  hasMore: boolean;
  warnings: string[];
}

async function composeMatches(opts: {
  platform: Platform;
  puuid: string;
  start: number;
  count: number;
  queue?: number;
  type?: string;
}): Promise<Composed<MatchPageBody>> {
  const { platform, puuid, start, count, queue, type } = opts;
  const region = platformToRegion(platform);

  // The id page is the one part that cannot fail softly: no ids, no matches.
  const ids = await fetcher.fetch<string[]>(
    build.matchIdsByPuuid(region, puuid, { start, count, queue, type }),
  );
  const matchIds = Array.isArray(ids.data) ? ids.data : [];

  const settled = await Promise.allSettled(
    matchIds.map((id) => fetcher.fetch(build.matchById(region, id))),
  );

  const warnings: string[] = [];
  const part = collector(warnings);
  // Failed matches are dropped rather than left as holes; `warnings[]` names
  // each one, so a caller that cares can still tell what is missing.
  const matches: unknown[] = [];
  settled.forEach((result, index) => {
    const match = part(`match ${matchIds[index]}`, result);
    if (match !== null) matches.push(match);
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
    warnings,
  };

  return { body, ...summarise([ids, ...fulfilled(settled)]) };
}

export default playerRoutes;

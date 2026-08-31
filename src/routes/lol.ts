import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { config } from '../config.js';
import { latestPatch, listChampionStats } from '../db/analytics.js';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { build } from '../riot/endpoints.js';
import {
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
} from '../riot/ladder.js';
import { assertPlatform, assertRegion, regionFromMatchId } from '../riot/routing.js';
import { send } from './helpers.js';
import {
  ApexTierParam,
  ChampionStatsQuery,
  ChampionStatsResponse,
  DivisionParam,
  LadderPageQuery,
  LadderTierParam,
  MasteryQuery,
  MatchIdParam,
  MatchIdsQuery,
  PassthroughResponse,
  PlatformParam,
  PuuidParam,
  RankedQueueParam,
  RegionParam,
  localErrors,
  upstreamErrors,
} from './schemas.js';

/** §6.2 — the LoL surface. Every handler is a thin shell over `fetcher.fetch`. */
const lolRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/lol/summoners/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      return send(
        reply,
        await fetcher.fetch(build.summonerByPuuid(assertPlatform(platform), puuid)),
      );
    },
  );

  fastify.get(
    '/v1/lol/league/entries/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      return send(
        reply,
        await fetcher.fetch(build.leagueEntriesByPuuid(assertPlatform(platform), puuid)),
      );
    },
  );

  /**
   * The ladder, in the two shapes Riot serves it. Both are passthroughs like
   * everything else here; the crawl (#88) drives the same builders from the
   * worker at bulk priority rather than through these routes.
   */
  fastify.get(
    '/v1/lol/league/apex/:platform/:tier/:queue',
    {
      schema: {
        tags: ['lol'],
        summary: 'Read a whole apex league',
        params: Type.Object({
          platform: PlatformParam,
          tier: ApexTierParam,
          queue: RankedQueueParam,
        }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, tier, queue } = request.params as {
        platform: string;
        tier: string;
        queue: string;
      };
      return send(
        reply,
        await fetcher.fetch(
          build.apexLeague(
            assertPlatform(platform),
            assertApexTier(tier),
            assertRankedQueue(queue),
          ),
        ),
      );
    },
  );

  fastify.get(
    '/v1/lol/league/entries/:platform/:queue/:tier/:division',
    {
      schema: {
        tags: ['lol'],
        summary: 'Walk one page of a tier and division',
        description:
          'One ~205-entry page of the ladder. Pages are 1-based; a page past the end of the ' +
          'division returns an empty array rather than a 404.',
        params: Type.Object({
          platform: PlatformParam,
          queue: RankedQueueParam,
          tier: LadderTierParam,
          division: DivisionParam,
        }),
        querystring: LadderPageQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, queue, tier, division } = request.params as {
        platform: string;
        queue: string;
        tier: string;
        division: string;
      };
      const { page } = request.query as { page?: number };
      return send(
        reply,
        await fetcher.fetch(
          build.leagueEntriesByTier(
            assertPlatform(platform),
            assertRankedQueue(queue),
            assertPagedTier(tier),
            assertDivision(division),
            page,
          ),
        ),
      );
    },
  );

  fastify.get(
    '/v1/lol/matches/ids/:region/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, puuid: PuuidParam }),
        querystring: MatchIdsQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, puuid } = request.params as { region: string; puuid: string };
      const query = request.query as Record<string, number | string | undefined>;
      return send(
        reply,
        await fetcher.fetch(build.matchIdsByPuuid(assertRegion(region), puuid, query)),
      );
    },
  );

  /**
   * Match + timeline are immutable: `fetcher` checks the Postgres archive
   * before Redis or Riot (Phase 5), so an archived match costs zero quota.
   */
  fastify.get(
    '/v1/lol/matches/:region/:matchId',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, matchId: MatchIdParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, matchId } = request.params as { region: string; matchId: string };
      return send(
        reply,
        await fetcher.fetch(build.matchById(resolveMatchRegion(region, matchId), matchId)),
      );
    },
  );

  fastify.get(
    '/v1/lol/matches/:region/:matchId/timeline',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, matchId: MatchIdParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, matchId } = request.params as { region: string; matchId: string };
      return send(
        reply,
        await fetcher.fetch(build.matchTimeline(resolveMatchRegion(region, matchId), matchId)),
      );
    },
  );

  fastify.get(
    '/v1/lol/spectator/active/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      // A 404 here means "not in game" and is negative-cached for 30 s (§8.3).
      return send(reply, await fetcher.fetch(build.activeGame(assertPlatform(platform), puuid)));
    },
  );

  fastify.get(
    '/v1/lol/mastery/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        querystring: MasteryQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      const { top } = request.query as { top?: number };
      const p = assertPlatform(platform);
      const req = top ? build.masteryTopByPuuid(p, puuid, top) : build.masteryByPuuid(p, puuid);
      return send(reply, await fetcher.fetch(req));
    },
  );

  fastify.get(
    '/v1/lol/rotations/:platform',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.championRotations(assertPlatform(platform))));
    },
  );

  /**
   * What the crawl is for: pick and win rates per champion, at a tier.
   *
   * The one route in this file that never touches Riot — it reads
   * `champion_stats`, which a `maintenance` job recomputes from the archive
   * after a crawl completes. Empty until a crawl has run and been aggregated,
   * which is the honest answer rather than a 404: the endpoint exists, the
   * numbers do not yet.
   */
  fastify.get(
    '/v1/lol/analytics/champions',
    {
      schema: {
        tags: ['lol'],
        summary: 'Champion pick and win rates by tier',
        description:
          'Aggregated from the match archive, with each participant placed at the tier the ' +
          'latest ladder crawl found them at. Recomputed per (platform, queue) when a crawl ' +
          'completes.',
        querystring: ChampionStatsQuery,
        response: { 200: ChampionStatsResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const query = request.query as {
        platform?: string;
        queue?: string;
        tier?: string;
        patch?: string;
        limit?: number;
      };
      const platform = assertPlatform(query.platform ?? config.DEFAULT_PLATFORM);
      const queue = assertRankedQueue(query.queue ?? config.ladderQueues[0] ?? 'RANKED_SOLO_5x5');
      const tier = query.tier ? assertTier(query.tier) : undefined;
      // Newest aggregated patch by default: asking for "champion win rates"
      // without a patch means the current one, not every patch ever archived
      // averaged into a single meaningless number.
      const patch = query.patch ?? (await latestPatch(platform, queue));

      const rows = patch
        ? await listChampionStats({
            platform,
            queue,
            patch,
            ...(tier ? { tier } : {}),
            ...(query.limit ? { limit: query.limit } : {}),
          })
        : [];

      const totalGames = rows.reduce((total, row) => total + row.games, 0);
      // A recompute writes the whole slice at once, so any row's stamp is the
      // slice's; the newest is taken in case a partial write is ever visible.
      const computedAt = rows.reduce<Date | null>(
        (newest, row) => (!newest || row.computedAt > newest ? row.computedAt : newest),
        null,
      );

      // Derived from immutable archive rows, and only ever replaced wholesale
      // by a recompute — so it is safe to hold, and holding it is what keeps a
      // dashboard polling this off the database.
      reply.header('Cache-Control', 'public, max-age=300');

      return {
        platform,
        queue,
        tier: tier ?? null,
        patch: patch ?? null,
        computedAt: computedAt ? computedAt.toISOString() : null,
        totalGames,
        champions: rows.map((row) => ({
          championId: row.championId,
          tier: row.tier,
          patch: row.patch,
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
          share: totalGames > 0 ? round(row.games / totalGames) : 0,
        })),
      };
    },
  );

  fastify.get(
    '/v1/lol/status/:platform',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.platformStatus(assertPlatform(platform))));
    },
  );
};

/** Four decimal places: enough for a win rate, short of implying precision. */
function round(value: number): number {
  return Math.round(value * 10_000) / 10_000;
}

/**
 * Match IDs carry their own platform prefix (`EUW1_…`). When the caller's
 * region disagrees with the ID, the ID wins — routing a match to the wrong
 * regional host is a guaranteed 404.
 */
function resolveMatchRegion(region: string, matchId: string) {
  const declared = assertRegion(region);
  const derived = regionFromMatchId(matchId);
  if (derived && derived !== declared) {
    throw ProxyError.badRegion(
      `Match ${matchId} belongs to region '${derived}', not '${declared}'`,
    );
  }
  return derived ?? declared;
}

export default lolRoutes;

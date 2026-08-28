import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { build } from '../riot/endpoints.js';
import { assertPlatform, assertRegion, regionFromMatchId } from '../riot/routing.js';
import { send } from './helpers.js';
import {
  MasteryQuery,
  MatchIdParam,
  MatchIdsQuery,
  PassthroughResponse,
  PlatformParam,
  PuuidParam,
  RegionParam,
  errorResponses,
} from './schemas.js';

/** §6.2 — the LoL surface. Every handler is a thin shell over `fetcher.fetch`. */
const lolRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/lol/summoners/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
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

  fastify.get(
    '/v1/lol/matches/ids/:region/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, puuid: PuuidParam }),
        querystring: MatchIdsQuery,
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
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
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.championRotations(assertPlatform(platform))));
    },
  );

  fastify.get(
    '/v1/lol/status/:platform',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.platformStatus(assertPlatform(platform))));
    },
  );
};

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

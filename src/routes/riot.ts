import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { fetcher } from '../fetcher.js';
import { build } from '../riot/endpoints.js';
import { assertRegion } from '../riot/routing.js';
import { send } from './helpers.js';
import {
  GameNameParam,
  PassthroughResponse,
  PuuidParam,
  RegionParam,
  TagLineParam,
  errorResponses,
} from './schemas.js';

/**
 * §6.2 — account-v1. FR-2: this is the canonical entry point for resolving a
 * player; the deprecated summoner-by-name lookup is deliberately absent.
 */
const riotRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/riot/accounts/by-riot-id/:region/:gameName/:tagLine',
    {
      schema: {
        tags: ['riot'],
        params: Type.Object({
          region: RegionParam,
          gameName: GameNameParam,
          tagLine: TagLineParam,
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { region, gameName, tagLine } = request.params as {
        region: string;
        gameName: string;
        tagLine: string;
      };
      const result = await fetcher.fetch(
        build.accountByRiotId(assertRegion(region), gameName, tagLine),
      );
      return send(reply, result);
    },
  );

  fastify.get(
    '/v1/riot/accounts/by-puuid/:region/:puuid',
    {
      schema: {
        tags: ['riot'],
        params: Type.Object({ region: RegionParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { region, puuid } = request.params as { region: string; puuid: string };
      const result = await fetcher.fetch(build.accountByPuuid(assertRegion(region), puuid));
      return send(reply, result);
    },
  );
};

export default riotRoutes;

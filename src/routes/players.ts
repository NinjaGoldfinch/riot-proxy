import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { config } from '../config.js';
import { ProxyError } from '../errors.js';
import { fetcher, type FetchResult } from '../fetcher.js';
import { logger } from '../logger.js';
import { build } from '../riot/endpoints.js';
import { assertPlatform, platformToRegion } from '../riot/routing.js';
import { applyCacheHeaders } from './helpers.js';
import { PassthroughResponse, PlatformParam, PuuidParam, errorResponses } from './schemas.js';

/**
 * §6.3 — the composite endpoint, the proxy's biggest ergonomic win: one client
 * call fans out to four Riot calls server-side, each individually cached.
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
      const region = platformToRegion(platform);

      const warnings: string[] = [];

      // Fan out concurrently; per-part caching still applies (§6.3).
      const [account, summoner, league, mastery] = await Promise.allSettled([
        fetcher.fetch(build.accountByPuuid(region, puuid)),
        fetcher.fetch(build.summonerByPuuid(platform, puuid)),
        fetcher.fetch(build.leagueEntriesByPuuid(platform, puuid)),
        fetcher.fetch(build.masteryTopByPuuid(platform, puuid, topMastery)),
      ]);

      const part = <T>(name: string, settled: PromiseSettledResult<FetchResult<T>>): T | null => {
        if (settled.status === 'fulfilled') return settled.value.data;
        const reason = settled.reason;
        const message =
          reason instanceof ProxyError ? `${reason.code}: ${reason.message}` : 'unavailable';
        warnings.push(`${name} unavailable (${message})`);
        logger.debug({ part: name, err: reason }, 'composite part failed');
        return null;
      };

      const body = {
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

      // The composite is as fresh as its stalest successful part.
      const settled = [account, summoner, league, mastery].filter(
        (s): s is PromiseFulfilledResult<FetchResult<unknown>> => s.status === 'fulfilled',
      );
      const age = Math.max(0, ...settled.map((s) => s.value.ageSeconds));
      const anyMiss = settled.some((s) => s.value.cache === 'MISS');
      applyCacheHeaders(reply, anyMiss ? 'MISS' : 'HIT', age);

      return body;
    },
  );
};

export default playerRoutes;

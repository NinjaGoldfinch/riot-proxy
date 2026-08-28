import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { cacheKey } from '../cache/keys.js';
import { cacheStore } from '../cache/store.js';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { endpoint, type BuiltRequest, type MethodId } from '../riot/endpoints.js';
import { METHOD_IDS } from '../riot/endpoints.js';
import { isPlatform, isRegion, platformHost, regionHost } from '../riot/routing.js';
import { applyCacheHeaders } from './helpers.js';
import { PassthroughResponse, errorResponses } from './schemas.js';

/**
 * Phase 1 — a raw passthrough for manual testing against a dev key. Admin
 * scope only: it can reach any Riot path, which is exactly what you want while
 * debugging and exactly what you do not want exposed to consumers.
 */
const debugRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/admin/debug/riot',
    {
      config: { scope: 'admin' },
      schema: {
        tags: ['admin'],
        querystring: Type.Object({
          scope: Type.String({ minLength: 2, maxLength: 12 }),
          path: Type.String({ minLength: 1, maxLength: 400 }),
          method: Type.Optional(Type.Unsafe<string>({ type: 'string', enum: [...METHOD_IDS] })),
          noCache: Type.Optional(Type.Boolean({ default: false })),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request, reply) => {
      const { scope, path, method, noCache } = request.query as {
        scope: string;
        path: string;
        method?: string;
        noCache?: boolean;
      };

      if (!path.startsWith('/')) {
        throw new ProxyError('VALIDATION', "path must start with '/'");
      }

      const lower = scope.toLowerCase();
      const host = isRegion(lower)
        ? regionHost(lower)
        : isPlatform(lower)
          ? platformHost(lower)
          : undefined;
      if (!host) throw ProxyError.badRegion(`'${scope}' is neither a platform nor a region`);

      // Split off the query string so the canonical cache key sorts it (§8.1).
      const [rawPath = path, rawQuery = ''] = path.split('?');
      const query: Record<string, string> = {};
      for (const [k, v] of new URLSearchParams(rawQuery)) query[k] = v;

      // Unknown paths borrow a conservative bucket so debug traffic still
      // counts against the limiter rather than bypassing it.
      const methodId = (method ?? 'status.platformData') as MethodId;

      const req: BuiltRequest = {
        method: methodId,
        host,
        path: rawPath,
        query,
        scope: lower,
        spec: endpoint(methodId),
      };

      const result = await fetcher.fetch(req, { bypassCache: noCache ?? false });
      applyCacheHeaders(reply, result.cache, result.ageSeconds);
      return result.data;
    },
  );

  /** Inspect what the proxy has cached for a built request, without fetching. */
  fastify.get(
    '/v1/admin/debug/cache',
    {
      config: { scope: 'admin' },
      schema: {
        tags: ['admin'],
        querystring: Type.Object({
          scope: Type.String(),
          path: Type.String(),
          method: Type.Unsafe<string>({ type: 'string', enum: [...METHOD_IDS] }),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const { scope, path, method } = request.query as {
        scope: string;
        path: string;
        method: string;
      };
      const lower = scope.toLowerCase();
      const host = isRegion(lower) ? regionHost(lower) : platformHost(lower as never);
      const key = cacheKey({
        method: method as MethodId,
        host,
        path,
        query: {},
        scope: lower,
        spec: endpoint(method as MethodId),
      });
      const entry = await cacheStore.get(key);
      return {
        key,
        present: entry !== undefined,
        ageSeconds: entry?.ageSeconds ?? null,
        stale: entry?.stale ?? null,
      };
    },
  );
};

export default debugRoutes;

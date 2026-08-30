import type { FastifyPluginAsync } from 'fastify';
import { pingDb } from '../db/index.js';
import { registry } from '../metrics.js';
import { redis } from '../redis.js';
import { limiter } from '../riot/limiter.js';
import { config } from '../config.js';
import {
  HealthResponse,
  LimitsResponse,
  ReadyResponse,
  ScopeParam,
  localErrors,
} from './schemas.js';
import { Type } from '@sinclair/typebox';

/** §6.2 / FR-12 — liveness and Prometheus. Both are public (no consumer key). */
const healthRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/healthz',
    {
      config: { public: true },
      schema: {
        tags: ['ops'],
        summary: 'Liveness',
        description:
          'Answers as long as the process is up. It touches neither Redis nor Postgres, so a ' +
          '200 here says only that the event loop is turning — use `/readyz` to decide whether ' +
          'to send traffic.',
        response: { 200: HealthResponse },
      },
    },
    async () => {
      return { ok: true };
    },
  );

  // Deeper check for load balancers and the compose healthcheck.
  fastify.get(
    '/readyz',
    {
      config: { public: true },
      schema: {
        tags: ['ops'],
        summary: 'Readiness',
        description:
          'Pings Redis and Postgres. **The 503 is the interesting response**: it carries the ' +
          'same body as the 200, so the `redis` and `postgres` booleans name which dependency ' +
          'is the reason the service is not ready. Used by the compose healthcheck and by any ' +
          'load balancer that should stop routing rather than serve errors.',
        response: { 200: ReadyResponse, 503: ReadyResponse },
      },
    },
    async (_request, reply) => {
      const [redisOk, dbOk] = await Promise.all([
        redis
          .ping()
          .then(() => true)
          .catch(() => false),
        pingDb(),
      ]);
      const ok = redisOk && dbOk;
      if (!ok) reply.code(503);
      return { ok, redis: redisOk, postgres: dbOk, keyScope: config.KEY_SCOPE };
    },
  );

  fastify.get(
    '/metrics',
    {
      config: { public: true },
      schema: {
        tags: ['ops'],
        summary: 'Prometheus metrics',
        description:
          'The §13 metric set in Prometheus text exposition format — not JSON. In the shipped ' +
          'Caddy configuration this path is reachable only from private ranges, so a public ' +
          'deployment will see a 403 from the proxy in front rather than a response from here.',
        // Declared as `content` rather than a bare schema: a response schema
        // keyed by status alone would hand this route to fast-json-stringify,
        // which would JSON-encode the exposition text.
        response: {
          200: {
            description: 'Prometheus text exposition format',
            content: { 'text/plain': { schema: Type.String() } },
          },
        },
      },
    },
    async (_request, reply) => {
      reply.header('Content-Type', registry.contentType);
      return registry.metrics();
    },
  );

  // Operator visibility into current bucket usage (§9) without a Grafana trip.
  fastify.get(
    '/v1/admin/limits/:scope',
    {
      config: { scope: 'admin' },
      schema: {
        tags: ['admin'],
        summary: 'Rate-limit bucket usage',
        description:
          'Current token usage for one Riot rate-limit bucket, and how long any 429-induced ' +
          'freeze on it has left to run. The scope is a host, not a game region: platform ' +
          'endpoints bucket by platform (`euw1`), account-v1 and match-v5 by region (`europe`).',
        params: Type.Object({ scope: ScopeParam }),
        response: { 200: LimitsResponse, ...localErrors },
      },
    },
    async (request) => {
      const { scope } = request.params as { scope: string };
      const [usage, frozenMs] = await Promise.all([limiter.usage(scope), limiter.isFrozen(scope)]);
      return { scope, usage, frozenMs };
    },
  );
};

export default healthRoutes;

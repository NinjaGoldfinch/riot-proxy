import type { FastifyPluginAsync } from 'fastify';
import { pingDb } from '../db/index.js';
import { registry } from '../metrics.js';
import { redis } from '../redis.js';
import { limiter } from '../riot/limiter.js';
import { config } from '../config.js';

/** §6.2 / FR-12 — liveness and Prometheus. Both are public (no consumer key). */
const healthRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get('/healthz', { config: { public: true } }, async () => {
    return { ok: true };
  });

  // Deeper check for load balancers and the compose healthcheck.
  fastify.get('/readyz', { config: { public: true } }, async (_request, reply) => {
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
  });

  fastify.get('/metrics', { config: { public: true } }, async (_request, reply) => {
    reply.header('Content-Type', registry.contentType);
    return registry.metrics();
  });

  // Operator visibility into current bucket usage (§9) without a Grafana trip.
  fastify.get('/v1/admin/limits/:scope', { config: { scope: 'admin' } }, async (request) => {
    const { scope } = request.params as { scope: string };
    const [usage, frozenMs] = await Promise.all([limiter.usage(scope), limiter.isFrozen(scope)]);
    return { scope, usage, frozenMs };
  });
};

export default healthRoutes;

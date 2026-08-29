import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { invalidateAuthCache } from '../auth/plugin.js';
import { cacheStore } from '../cache/store.js';
import { config } from '../config.js';
import {
  createConsumer,
  disableConsumer,
  findConsumerByHash,
  listConsumers,
} from '../db/consumers.js';
import { countArchivedMatches } from '../db/matches.js';
import { listPlayers, setTracked, upsertPlayer } from '../db/players.js';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { logger } from '../logger.js';
import { build } from '../riot/endpoints.js';
import { assertPlatform, platformToAccountRegion } from '../riot/routing.js';
import { JOB, backfillQueue, ddragonQueue, type BackfillPlayerJob } from '../jobs/queues.js';
import {
  GameNameParam,
  PassthroughResponse,
  PlatformParam,
  PuuidParam,
  TagLineParam,
  errorResponses,
} from './schemas.js';

/**
 * FR-14 / §6.2 — admin surface. Every route here requires the `admin` scope and
 * passes the IP allowlist check in the auth hook (§12.1).
 */
const adminScope = { config: { scope: 'admin' as const } };

const adminRoutes: FastifyPluginAsync = async (fastify) => {
  // ── consumers ──────────────────────────────────────────────────────────────

  fastify.post(
    '/v1/admin/consumers',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        body: Type.Object({
          name: Type.String({ minLength: 1, maxLength: 100 }),
          scopes: Type.Optional(
            Type.Array(Type.Unsafe<string>({ type: 'string', enum: ['read', 'admin'] }), {
              minItems: 1,
            }),
          ),
          quotaPerMin: Type.Optional(Type.Integer({ minimum: 1, maximum: 1_000_000 })),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const body = request.body as { name: string; scopes?: string[]; quotaPerMin?: number };
      const created = await createConsumer({
        name: body.name,
        ...(body.scopes ? { scopes: body.scopes } : {}),
        ...(body.quotaPerMin !== undefined ? { quotaPerMin: body.quotaPerMin } : {}),
      });
      if (!created) throw new ProxyError('INTERNAL', 'Failed to create consumer');
      logger.info({ id: created.id, name: created.name }, 'consumer created');
      // The plaintext key is returned exactly once and never stored (§12.1).
      return { ...created, warning: 'Store this key now — it cannot be retrieved again.' };
    },
  );

  fastify.get('/v1/admin/consumers', adminScope, async () => ({
    consumers: await listConsumers(),
  }));

  fastify.delete(
    '/v1/admin/consumers/:id',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ id: Type.String({ format: 'uuid' }) }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const { id } = request.params as { id: string };
      const disabled = await disableConsumer(id);
      if (!disabled) throw ProxyError.notFound('No active consumer with that id');
      logger.info({ id }, 'consumer disabled');
      return { ok: true, id };
    },
  );

  // ── tracked players ────────────────────────────────────────────────────────

  fastify.get('/v1/admin/tracked-players', adminScope, async () => ({
    players: await listPlayers(),
  }));

  /**
   * Accepts either a PUUID or a Riot ID. Resolving the Riot ID here means the
   * caller never has to know the PUUID, and the resolution goes through the
   * normal cached read path (FR-2).
   */
  fastify.post(
    '/v1/admin/tracked-players',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        body: Type.Object({
          platform: PlatformParam,
          puuid: Type.Optional(PuuidParam),
          gameName: Type.Optional(GameNameParam),
          tagLine: Type.Optional(TagLineParam),
          tracked: Type.Optional(Type.Boolean({ default: true })),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const body = request.body as {
        platform: string;
        puuid?: string;
        gameName?: string;
        tagLine?: string;
        tracked?: boolean;
      };
      const platform = assertPlatform(body.platform);

      let puuid = body.puuid;
      let gameName = body.gameName ?? null;
      let tagLine = body.tagLine ?? null;

      if (!puuid) {
        if (!body.gameName || !body.tagLine) {
          throw new ProxyError('VALIDATION', 'Provide either puuid, or gameName and tagLine');
        }
        const region = platformToAccountRegion(platform);
        const { data } = await fetcher.fetch<{
          puuid: string;
          gameName?: string;
          tagLine?: string;
        }>(build.accountByRiotId(region, body.gameName, body.tagLine));
        puuid = data.puuid;
        gameName = data.gameName ?? body.gameName;
        tagLine = data.tagLine ?? body.tagLine;
      }

      const player = await upsertPlayer({
        puuid,
        platform,
        gameName,
        tagLine,
        tracked: body.tracked ?? true,
      });
      logger.info({ puuid: player.puuid, tracked: player.tracked }, 'tracked player upserted');
      return player;
    },
  );

  fastify.delete(
    '/v1/admin/tracked-players/:puuid',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const { puuid } = request.params as { puuid: string };
      // Untrack rather than delete: the row still carries useful identity data.
      const ok = await setTracked(puuid, false);
      if (!ok) throw ProxyError.notFound('No such player for the current key scope');
      return { ok: true, puuid, tracked: false };
    },
  );

  // ── cache ──────────────────────────────────────────────────────────────────

  fastify.post(
    '/v1/admin/cache/purge',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        body: Type.Object({
          pattern: Type.String({ minLength: 1, maxLength: 200 }),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const { pattern } = request.body as { pattern: string };
      const deleted = await cacheStore.purge(pattern);
      return { ok: true, pattern, deleted };
    },
  );

  // ── jobs ───────────────────────────────────────────────────────────────────

  fastify.post(
    '/v1/admin/backfill',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        body: Type.Object({
          puuid: PuuidParam,
          platform: PlatformParam,
          limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 10_000, default: 500 })),
          fetchTimeline: Type.Optional(Type.Boolean({ default: false })),
        }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const body = request.body as BackfillPlayerJob;
      assertPlatform(body.platform);
      const job = await backfillQueue.add(JOB.backfillPlayer, body, {
        jobId: `backfill:${body.puuid}`,
      });
      return { ok: true, jobId: job.id };
    },
  );

  fastify.post(
    '/v1/admin/ddragon/sync',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        body: Type.Optional(
          Type.Object({ force: Type.Optional(Type.Boolean({ default: false })) }),
        ),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const body = (request.body ?? {}) as { force?: boolean };
      const job = await ddragonQueue.add(JOB.ddragonSync, { force: body.force ?? false });
      return { ok: true, jobId: job.id };
    },
  );

  // ── status ─────────────────────────────────────────────────────────────────

  fastify.get('/v1/admin/stats', adminScope, async () => ({
    keyScope: config.KEY_SCOPE,
    archivedMatches: await countArchivedMatches(),
    trackedPlayers: (await listPlayers()).filter((p) => p.tracked).length,
  }));

  /** Revoking a key must take effect immediately, not after the 300 s auth TTL. */
  fastify.post(
    '/v1/admin/consumers/:id/revoke-cache',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ id: Type.String({ format: 'uuid' }) }),
        body: Type.Object({ keyHash: Type.String({ minLength: 64, maxLength: 64 }) }),
        response: { 200: PassthroughResponse, ...errorResponses },
      },
    },
    async (request) => {
      const { keyHash } = request.body as { keyHash: string };
      const exists = await findConsumerByHash(keyHash);
      await invalidateAuthCache(keyHash);
      return { ok: true, stillActive: Boolean(exists) };
    },
  );
};

export default adminRoutes;

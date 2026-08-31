import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { invalidateAuthCache } from '../auth/plugin.js';
import { cacheStore } from '../cache/store.js';
import { config } from '../config.js';
import {
  createConsumer,
  disableConsumer,
  findConsumerByIdAndHash,
  listConsumers,
} from '../db/consumers.js';
import { getCrawl, finishCrawl, listCrawls } from '../db/ladder.js';
import { countArchivedMatches } from '../db/matches.js';
import type { LadderCrawl } from '../db/schema.js';
import { countTrackedPlayers, listPlayers, setTracked, upsertPlayer } from '../db/players.js';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { logger } from '../logger.js';
import { build } from '../riot/endpoints.js';
import { assertPlatform, platformToAccountRegion } from '../riot/routing.js';
import { clearCrawlState, pendingLegs } from '../jobs/ladder-state.js';
import { startCrawl } from '../jobs/processors.js';
import {
  JOB,
  enqueueBackfill,
  ddragonQueue,
  ladderQueue,
  type BackfillEnqueueResult,
  type BackfillPlayerJob,
} from '../jobs/queues.js';
import { METRICS_HISTORY_MAX_POINTS, readMetricsHistory } from '../stats/history.js';
import { buildMetricsSnapshot } from '../stats/snapshot.js';
import { wsHub } from '../ws/index.js';
import {
  AdminStatsResponse,
  ConsumerListResponse,
  GameNameParam,
  LadderCrawlBody,
  LadderCrawlListResponse,
  LadderCrawlStartedResponse,
  RankedQueueParam,
  MetricsHistoryResponse,
  MetricsResponse,
  PassthroughResponse,
  PlatformParam,
  PlayerListResponse,
  PuuidParam,
  TagLineParam,
  localErrors,
  upstreamErrors,
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
        response: { 200: PassthroughResponse, ...localErrors },
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

  fastify.get(
    '/v1/admin/consumers',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'List consumers',
        description:
          'Every consumer, including revoked ones — `disabledAt` distinguishes them. The key ' +
          'itself is never returned: only its sha256 is stored, and not even that is exposed ' +
          'here. A plaintext key is shown exactly once, by `POST /v1/admin/consumers`.',
        response: { 200: ConsumerListResponse, ...localErrors },
      },
    },
    async () => ({ consumers: await listConsumers() }),
  );

  fastify.delete(
    '/v1/admin/consumers/:id',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ id: Type.String({ format: 'uuid' }) }),
        response: { 200: PassthroughResponse, ...localErrors },
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

  fastify.get(
    '/v1/admin/tracked-players',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'List known players',
        description:
          'Every player row for the current `keyScope`, tracked or not — tracking is a flag on ' +
          'the row, not a separate table, and an untracked row still carries the identity and ' +
          'backfill state a later lookup reuses. Rows written under a previous Riot key are ' +
          'not returned: PUUIDs are encrypted per key (§7.4).',
        response: { 200: PlayerListResponse, ...localErrors },
      },
    },
    async () => ({ players: await listPlayers() }),
  );

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
        // The one admin route that leaves the process: given a Riot ID rather
        // than a PUUID it resolves the account upstream, so it inherits Riot's
        // failures along with our own.
        response: { 200: PassthroughResponse, ...upstreamErrors },
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

      const tracked = body.tracked ?? true;
      const player = await upsertPlayer({ puuid, platform, gameName, tagLine, tracked });
      logger.info({ puuid: player.puuid, tracked: player.tracked }, 'tracked player upserted');

      // #46 — tracking only ever archived what the poller happened to catch
      // from then on, so a player tracked today had no history at all and
      // nothing to reconcile a gap against. Walk them the way a first lookup
      // does (#44), and for the same reason: matches are immutable, so this is
      // quota spent once. A player already walked is left alone.
      let backfill: BackfillEnqueueResult | null = null;
      if (tracked && !player.historyBackfilledAt && config.LOOKUP_BACKFILL_LIMIT > 0) {
        try {
          backfill = await enqueueBackfill({
            puuid,
            platform,
            limit: config.LOOKUP_BACKFILL_LIMIT,
            reason: 'track',
          });
          logger.info({ puuid, ...backfill }, 'queued backfill on track');
        } catch (err) {
          // Tracking succeeded. A queue that is down must not undo that.
          logger.warn({ err, puuid }, 'could not queue backfill on track');
        }
      }

      return { ...player, backfill };
    },
  );

  fastify.delete(
    '/v1/admin/tracked-players/:puuid',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...localErrors },
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

  // ── ladder ─────────────────────────────────────────────────────────────────

  /**
   * Trigger a crawl. 202, because the answer is "it is running", not "here is
   * the ladder" — the walk behind this takes minutes on a production key and
   * hours on a dev one.
   *
   * The fan-out happens inline rather than through a `ladder:crawl` job: it
   * makes no upstream calls, and doing it here is what lets the response carry
   * a crawl id the caller can then poll, instead of a job id that names
   * nothing in the database.
   */
  fastify.post(
    '/v1/admin/ladder/crawl',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Start a ladder crawl',
        body: LadderCrawlBody,
        response: { 202: LadderCrawlStartedResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const body = request.body as { platform?: string; queue?: string; tierFloor?: string };
      const { crawlId, created, legs } = await startCrawl({
        platform: body.platform ?? config.DEFAULT_PLATFORM,
        queue: body.queue ?? config.ladderQueues[0] ?? 'RANKED_SOLO_5x5',
        ...(body.tierFloor !== undefined ? { tierFloor: body.tierFloor } : {}),
      });

      // Not a 409: a second trigger has not failed, it has been told which
      // crawl is already answering its question. The caller polls the same id
      // either way, and `status` is what tells the two apart.
      return reply.code(202).send({
        crawlId,
        status: created ? 'started' : 'already-running',
        legs,
      });
    },
  );

  fastify.get(
    '/v1/admin/ladder/crawls',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Recent ladder crawls',
        querystring: Type.Object({
          platform: Type.Optional(PlatformParam),
          queue: Type.Optional(RankedQueueParam),
          limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 100, default: 20 })),
        }),
        response: { 200: LadderCrawlListResponse, ...localErrors },
      },
    },
    async (request) => {
      const { platform, queue, limit } = request.query as {
        platform?: string;
        queue?: string;
        limit?: number;
      };
      const crawls = await listCrawls({
        ...(platform ? { platform: assertPlatform(platform) } : {}),
        ...(queue ? { queue } : {}),
        ...(limit ? { limit } : {}),
      });
      return { crawls: await Promise.all(crawls.map(withPendingLegs)) };
    },
  );

  /**
   * Cancel. Three things have to happen and only two of them are the database:
   * the row is marked, the legs still queued are dropped, and the ones already
   * running notice on their own — a walk re-reads the crawl's status every ten
   * pages, because BullMQ cannot take a job back once a worker holds it.
   */
  fastify.delete(
    '/v1/admin/ladder/crawls/:id',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Cancel a running ladder crawl',
        params: Type.Object({ id: Type.String({ format: 'uuid' }) }),
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (request) => {
      const { id } = request.params as { id: string };
      const existing = await getCrawl(id);
      if (!existing) throw ProxyError.notFound('No such ladder crawl for the current key scope');

      const cancelled = await finishCrawl(id, 'cancelled');
      if (!cancelled) {
        throw new ProxyError('VALIDATION', `Crawl ${id} is already ${existing.status}`);
      }

      const dropped = await dropQueuedLegs(id);
      await clearCrawlState(id);
      logger.info({ crawlId: id, dropped }, 'ladder crawl cancelled');
      return { ok: true, crawlId: id, status: 'cancelled', droppedJobs: dropped };
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
        response: { 200: PassthroughResponse, ...localErrors },
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
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (request) => {
      const body = request.body as BackfillPlayerJob;
      assertPlatform(body.platform);

      const { jobId, status } = await enqueueBackfill(body);
      return { ok: true, jobId, status };
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
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (request) => {
      const body = (request.body ?? {}) as { force?: boolean };
      const job = await ddragonQueue.add(JOB.ddragonSync, { force: body.force ?? false });
      return { ok: true, jobId: job.id };
    },
  );

  // ── status ─────────────────────────────────────────────────────────────────

  fastify.get(
    '/v1/admin/stats',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Archive and tracking counts',
        description:
          '`archivedMatches` counts the whole archive, which is not key-scoped — match IDs are ' +
          'not encrypted, so it survives a key rotation. `trackedPlayers` is scoped to the ' +
          'current key and will read as zero immediately after one.',
        response: { 200: AdminStatsResponse, ...localErrors },
      },
    },
    async () => ({
      keyScope: config.KEY_SCOPE,
      archivedMatches: await countArchivedMatches(),
      trackedPlayers: await countTrackedPlayers(),
    }),
  );

  fastify.get(
    '/v1/admin/metrics',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Operational snapshot',
        description:
          'The same document the `metrics` WebSocket topic ticks, built by the same function — ' +
          'fetch this once on load, then subscribe for updates. The `ws`, `events` and `cache` ' +
          'sections are counted by the api process answering this request; everything else ' +
          '(archive counts, queue populations, limiter state) is shared state and identical ' +
          'from any instance.',
        response: { 200: MetricsResponse, ...localErrors },
      },
    },
    async () =>
      buildMetricsSnapshot({
        wsConnections: wsHub.size,
        wsSubscriptions: wsHub.subscriptionCount,
        wsEventCounts: wsHub.eventCounts,
      }),
  );

  fastify.get(
    '/v1/admin/metrics/history',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        summary: 'Metrics history',
        description:
          'Compact operational points recorded every `METRICS_HISTORY_INTERVAL_S` seconds ' +
          '(default 60) into a capped list — 1440 points, a day at the default cadence — ' +
          'whether or not anything is subscribed to the live topic. This is what lets a ' +
          'dashboard opened cold draw the last 24 h. The `cache` counters belong to whichever ' +
          'api process recorded each point: diff consecutive points for a rate, and read a ' +
          'negative step as that process restarting.',
        response: { 200: MetricsHistoryResponse, ...localErrors },
      },
    },
    async () => ({
      intervalS: config.METRICS_HISTORY_INTERVAL_S,
      maxPoints: METRICS_HISTORY_MAX_POINTS,
      points: await readMetricsHistory(),
    }),
  );

  /**
   * Revoking a key must take effect immediately, not after the 300 s auth TTL.
   *
   * The path names the consumer and the body names the key, and the two have to
   * agree: the handler used to read the body alone, so any uuid revoked any
   * hash and `stillActive` was an answer about whatever key was passed rather
   * than about the consumer in the URL. Admin-scoped and IP-allowlisted, so
   * this was never a way in — but the route promised a relationship it did not
   * enforce, and now it enforces it.
   */
  fastify.post(
    '/v1/admin/consumers/:id/revoke-cache',
    {
      ...adminScope,
      schema: {
        tags: ['admin'],
        params: Type.Object({ id: Type.String({ format: 'uuid' }) }),
        body: Type.Object({ keyHash: Type.String({ minLength: 64, maxLength: 64 }) }),
        response: { 200: PassthroughResponse, ...localErrors },
      },
    },
    async (request) => {
      const { id } = request.params as { id: string };
      const { keyHash } = request.body as { keyHash: string };

      const consumer = await findConsumerByIdAndHash(id, keyHash);
      if (!consumer) throw ProxyError.notFound('No consumer with that id and key hash');

      await invalidateAuthCache(keyHash);
      return { ok: true, id, stillActive: consumer.disabledAt === null };
    },
  );
};

/**
 * Remove the legs of a cancelled crawl that have not started. An active job
 * cannot be removed — BullMQ will not take work back from a worker that holds
 * the lock — which is what the walk's own status check is for.
 */
async function dropQueuedLegs(crawlId: string): Promise<number> {
  const jobs = await ladderQueue.getJobs(['waiting', 'delayed', 'prioritized']);
  let dropped = 0;
  for (const job of jobs) {
    if ((job.data as { crawlId?: string }).crawlId !== crawlId) continue;
    try {
      await job.remove();
      dropped += 1;
    } catch {
      // Claimed by a worker between the listing and the remove; it will stop
      // itself on the next status check.
    }
  }
  return dropped;
}

/**
 * How much of a crawl is left, read from Redis rather than stored on the row:
 * the set is written by the fan-out and drained by the legs themselves, so it
 * is the only place that knows without asking BullMQ about every job.
 */
async function withPendingLegs(crawl: LadderCrawl) {
  return { ...crawl, pendingLegs: crawl.status === 'running' ? await pendingLegs(crawl.id) : 0 };
}

export default adminRoutes;

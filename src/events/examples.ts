import type { EventName, ProxyEvent } from './index.js';
import { METRICS_TOPIC, PATCH_TOPIC, playerTopic } from './topics.js';

/**
 * One worked sample per event, for the reference (#74). §11 is documented as
 * markdown prose because OpenAPI 3.1 cannot express a WebSocket, and this is
 * what stops that prose describing a payload the service no longer publishes.
 *
 * The mapped type is the whole point: a field renamed in `EventPayloads` fails
 * to compile here, and a new event fails to compile until it has a sample — so
 * the documented shape cannot drift from the published one, and a consumer
 * parsing these is parsing the real thing.
 *
 * The import of `EventPayloads` is type-only and `./topics.js` reaches no
 * further than config, so this module stays free of the side effect that
 * matters: rendering the document must not open a connection to Redis.
 */
export const EVENT_EXAMPLES: { [E in EventName]: ProxyEvent<E> } = {
  'game.started': {
    event: 'game.started',
    topic: playerTopic('…'),
    at: 1_756_000_000_000,
    data: { puuid: '…', platform: 'euw1', gameId: 1_234_567_890, championId: 64, queueId: 420 },
  },
  'game.ended': {
    event: 'game.ended',
    topic: playerTopic('…'),
    at: 1_756_000_000_000,
    data: { puuid: '…', gameId: 1_234_567_890 },
  },
  'rank.changed': {
    event: 'rank.changed',
    topic: playerTopic('…'),
    at: 1_756_000_000_000,
    data: {
      puuid: '…',
      queue: 'RANKED_SOLO_5x5',
      before: { tier: 'GOLD', rank: 'I', lp: 98 },
      after: { tier: 'PLATINUM', rank: 'IV', lp: 12 },
    },
  },
  'match.archived': {
    event: 'match.archived',
    topic: playerTopic('…'),
    at: 1_756_000_000_000,
    data: { puuid: '…', matchId: 'EUW1_7381937461' },
  },
  'patch.new': {
    event: 'patch.new',
    // The one event that is not about a player, and the sample says so.
    topic: PATCH_TOPIC,
    at: 1_756_000_000_000,
    data: { version: '15.16.1' },
  },
  'metrics.snapshot': {
    event: 'metrics.snapshot',
    // Admin-only, like the topic itself. One queue and one limiter scope shown;
    // the real payload carries every queue and every scope the deployment has
    // talked to.
    topic: METRICS_TOPIC,
    at: 1_756_000_000_000,
    data: {
      v: 1,
      keyScope: 'a1b2c3d4',
      totals: {
        archivedMatches: 48_213,
        trackedPlayers: 12,
        knownPlayers: 3417,
        activeConsumers: 4,
      },
      queues: {
        backfill: {
          active: 2,
          waiting: 0,
          prioritized: 137,
          delayed: 0,
          failed: 1,
          completed: 812,
        },
      },
      ws: { connections: 3, subscriptions: 7 },
      events: { 'match.archived': 812, 'game.started': 41 },
      cache: { hit: 10_412, miss: 1_733, neg: 88, stale: 402 },
      limiter: [
        {
          scope: 'euw1',
          kind: 'platform',
          label: 'EU West',
          frozenMs: 0,
          windows: [
            { window: '20:1', used: 3, limit: 20 },
            { window: '100:120', used: 41, limit: 100 },
          ],
          methods: [
            {
              method: 'summoner.byPuuid',
              windows: [{ window: '1600:60', used: 12, limit: 1600 }],
            },
          ],
        },
      ],
      worker: { alive: true, lastSeenMs: 4_000 },
      flows: {
        backfillsQueued: { 'lookup:queued': 214, 'lookup:already-queued': 37, 'admin:queued': 3 },
        refreshClaims: { 'profile:claimed': 96, 'profile:coalesced': 311 },
      },
      process: { uptimeSeconds: 86_400, rssBytes: 182_000_000 },
    },
  },
};

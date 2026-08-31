import type { EventName, ProxyEvent } from './index.js';
import { PATCH_TOPIC, playerTopic } from './topics.js';

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
 * The import of `EventPayloads` is type-only and `./topics.js` has no imports
 * at all, so this module stays free of side effects: rendering the document
 * must not open a connection to Redis.
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
};

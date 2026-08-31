import { KEY_SCOPE } from '../config.js';

/**
 * §11 — what clients subscribe to, and how a topic maps onto a Redis channel.
 *
 * Split out of `src/events/index.ts` because that module holds `publish`, which
 * imports the Redis client and opens a connection on load. The config import
 * here keeps that property — parsing env is not opening a connection — and the
 * reference reads this module to describe the protocol (#74). Everything is
 * re-exported from `index.ts`, so nothing else has to care where it lives.
 */

/** Anything about one player. */
export function playerTopic(puuid: string): string {
  return `player:${puuid}`;
}

/** Global patch events, the same for every consumer. */
export const PATCH_TOPIC = 'patch';

/** Periodic operational snapshots, published while anyone is subscribed. */
export const METRICS_TOPIC = 'metrics';

/** Every event the service publishes, regardless of topic. */
export const FIREHOSE_TOPIC = 'firehose';

/**
 * Topics that expose operational internals — or, for the firehose, every
 * player's events at once — so holding them requires the `admin` scope. The
 * check is at subscribe time (`src/ws/index.ts`): a topic a socket can never
 * hold is one relay never has to re-authorise.
 */
export const ADMIN_TOPICS: ReadonlySet<string> = new Set([METRICS_TOPIC, FIREHOSE_TOPIC]);

/**
 * Channels are key-scoped like every other Redis key (§7.4), and it took a
 * while to notice they weren't: a test suite and a dev server sharing one
 * Redis published into each other's firehose, so the dev dashboard's event
 * feed showed the suite's `EUW1_1` fixtures every time the tests ran. Topics
 * stay unscoped — they are the client-facing names — the scope lives only on
 * the wire between publisher and relay.
 */
const CHANNEL_PREFIX = `evt:${KEY_SCOPE}:`;

export const channelFor = (topic: string) => `${CHANNEL_PREFIX}${topic}`;
export const CHANNEL_PATTERN = `${CHANNEL_PREFIX}*`;

export function topicFromChannel(channel: string): string {
  return channel.startsWith(CHANNEL_PREFIX) ? channel.slice(CHANNEL_PREFIX.length) : channel;
}

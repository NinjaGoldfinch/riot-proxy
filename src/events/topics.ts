/**
 * §11 — what clients subscribe to, and how a topic maps onto a Redis channel.
 *
 * Split out of `src/events/index.ts` because that module holds `publish`, which
 * imports the Redis client and opens a connection on load. These are pure
 * strings, and the reference reads them to describe the protocol (#74). Both
 * are re-exported from `index.ts`, so nothing else has to care where they live.
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

export const channelFor = (topic: string) => `evt:${topic}`;
export const CHANNEL_PATTERN = 'evt:*';

export function topicFromChannel(channel: string): string {
  return channel.startsWith('evt:') ? channel.slice(4) : channel;
}

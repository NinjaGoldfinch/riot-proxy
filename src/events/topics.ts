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

export const channelFor = (topic: string) => `evt:${topic}`;
export const CHANNEL_PATTERN = 'evt:*';

export function topicFromChannel(channel: string): string {
  return channel.startsWith('evt:') ? channel.slice(4) : channel;
}

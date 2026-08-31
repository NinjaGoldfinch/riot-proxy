import { logger } from '../logger.js';
import { publisher } from '../redis.js';
import type { MetricsSnapshotData } from '../stats/schema.js';
import { channelFor } from './topics.js';

/** §11 — the realtime event catalogue. */
export interface EventPayloads {
  'game.started': {
    puuid: string;
    platform: string;
    gameId: number;
    championId?: number;
    queueId?: number;
  };
  'game.ended': { puuid: string; gameId: number };
  'rank.changed': {
    puuid: string;
    queue: string;
    before: { tier?: string; rank?: string; lp?: number } | null;
    after: { tier?: string; rank?: string; lp?: number } | null;
  };
  'match.archived': { puuid?: string; matchId: string };
  'patch.new': { version: string };
  /**
   * A crawl finished cleanly. Carries the counters rather than a pointer to
   * them, so a dashboard can render the run without a follow-up call — and it
   * is what triggers the champion aggregate for that ladder.
   */
  'ladder.crawl.completed': {
    crawlId: string;
    platform: string;
    queue: string;
    entries: number;
    players: number;
    durationS: number;
  };
  'metrics.snapshot': MetricsSnapshotData;
}

export type EventName = keyof EventPayloads;

export interface ProxyEvent<E extends EventName = EventName> {
  event: E;
  topic: string;
  at: number;
  data: EventPayloads[E];
}

/**
 * Topics are what clients subscribe to (§11): `player:<puuid>` for anything
 * about one player, `patch` for global patch events. Defined in a leaf module
 * so the reference can read them; re-exported here because this is where the
 * rest of the service expects to find them.
 */
export {
  playerTopic,
  PATCH_TOPIC,
  METRICS_TOPIC,
  FIREHOSE_TOPIC,
  LADDER_TOPIC,
  ADMIN_TOPICS,
  channelFor,
  CHANNEL_PATTERN,
  topicFromChannel,
} from './topics.js';

/**
 * Worker → Redis → every api instance → local sockets. Publishing is
 * fire-and-forget: losing an event must never fail the job that produced it.
 */
export async function publish<E extends EventName>(
  event: E,
  topic: string,
  data: EventPayloads[E],
): Promise<void> {
  const payload: ProxyEvent<E> = { event, topic, at: Date.now(), data };
  try {
    await publisher.publish(channelFor(topic), JSON.stringify(payload));
    logger.debug({ event, topic }, 'event published');
  } catch (err) {
    logger.warn({ err, event, topic }, 'failed to publish event');
  }
}

export function parseEvent(raw: string): ProxyEvent | undefined {
  try {
    const parsed = JSON.parse(raw) as ProxyEvent;
    if (typeof parsed?.event === 'string' && typeof parsed?.topic === 'string') return parsed;
  } catch {
    // fall through
  }
  return undefined;
}

import { Redis, type RedisOptions } from 'ioredis';
import { config } from './config.js';
import { logger } from './logger.js';

const baseOptions: RedisOptions = {
  maxRetriesPerRequest: null, // required by BullMQ, harmless elsewhere
  enableReadyCheck: true,
  lazyConnect: false,
  retryStrategy: (times) => Math.min(times * 200, 5000),
};

function create(role: string, overrides: RedisOptions = {}): Redis {
  const client = new Redis(config.REDIS_URL, { ...baseOptions, ...overrides });
  client.on('error', (err: Error) => logger.error({ err, role }, 'redis connection error'));
  client.on('reconnecting', () => logger.warn({ role }, 'redis reconnecting'));
  return client;
}

/** Command connection: cache, limiter, auth lookups, quotas. */
export const redis = create('main');

/**
 * A connection in subscriber mode cannot issue normal commands, so the pub/sub
 * relay (§11) gets its own. Created lazily — the worker never needs it.
 */
let subscriberClient: Redis | undefined;
export function subscriber(): Redis {
  if (!subscriberClient) subscriberClient = create('subscriber');
  return subscriberClient;
}

/** Publisher shares the main connection; publishing is a normal command. */
export const publisher = redis;

export async function closeRedis(): Promise<void> {
  await Promise.allSettled([
    redis.quit(),
    subscriberClient ? subscriberClient.quit() : Promise.resolve(),
  ]);
}

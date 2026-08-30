import { parseArgs } from 'node:util';
import { config, KEY_SCOPE } from '../config.js';
import { closeRedis, redis } from '../redis.js';

/**
 * Local teardown — `npm run reset:cache`. Everything the proxy caches is
 * derivable again from Riot, so dropping it costs nothing but the re-fetch.
 *
 * Deletes are scoped to this deployment's `KEY_SCOPE` (§7.4) by default, so a
 * Redis shared with another proxy — or with the previous Riot key's namespace
 * — keeps its keys.
 */

const USAGE = `Usage: npm run reset:cache [-- --limiter] [--all]

  (default)   cached responses, negative markers and single-flight locks
  --limiter   also the learned rate-limit buckets and their config
  --all       FLUSHDB: the above plus BullMQ queues, consumer quotas, the
              auth cache and worker poll state, for every key scope
  --force     allow the reset while NODE_ENV=production`;

/** SCAN rather than KEYS so a large namespace does not block Redis (§6.2). */
async function deleteMatching(pattern: string): Promise<number> {
  let cursor = '0';
  let deleted = 0;
  do {
    const [next, keys] = await redis.scan(cursor, 'MATCH', pattern, 'COUNT', 500);
    cursor = next;
    if (keys.length > 0) deleted += await redis.del(...keys);
  } while (cursor !== '0');
  return deleted;
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      limiter: { type: 'boolean', default: false },
      all: { type: 'boolean', default: false },
      force: { type: 'boolean', default: false },
      help: { type: 'boolean', short: 'h', default: false },
    },
  });

  if (values.help) {
    console.log(USAGE);
    return;
  }

  if (config.isProduction && !values.force) {
    console.error('Refusing to reset cache with NODE_ENV=production. Pass --force if you mean it.');
    process.exitCode = 1;
    return;
  }

  if (values.all) {
    await redis.flushdb();
    console.log(`Flushed ${config.REDIS_URL} — every key, every scope.`);
    return;
  }

  const patterns = [`c:${KEY_SCOPE}:*`, `neg:${KEY_SCOPE}:*`, `sf:c:${KEY_SCOPE}:*`];
  // Limiter keys carry the scope in the middle of the key, not at the front:
  // `rl:app:{version}:{scope}:...`. One glob covers every rl: shape.
  if (values.limiter) patterns.push(`rl:*${KEY_SCOPE}*`);

  let total = 0;
  for (const pattern of patterns) {
    const deleted = await deleteMatching(pattern);
    total += deleted;
    console.log(`  ${String(deleted).padStart(7)}  ${pattern}`);
  }
  console.log(`\nDeleted ${total} keys for key scope ${KEY_SCOPE}.`);
}

main()
  .catch((err: unknown) => {
    console.error(err);
    process.exitCode = 1;
  })
  .finally(() => closeRedis());

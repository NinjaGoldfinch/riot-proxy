import { buildApp } from './app.js';
import { config } from './config.js';
import { closeDb } from './db/index.js';
import { logger } from './logger.js';
import { closeRedis } from './redis.js';
import { riotClient } from './riot/client.js';
import { wsHub } from './ws/index.js';

/** `api` process (§3.3): HTTP + WS surface, cache reads, upstream fetches. */
async function main(): Promise<void> {
  const app = await buildApp();

  await app.listen({ port: config.PORT, host: config.HOST });
  logger.info(
    { port: config.PORT, keyScope: config.KEY_SCOPE, env: config.NODE_ENV },
    'riot-proxy api listening',
  );

  const shutdown = async (signal: string) => {
    logger.info({ signal }, 'shutting down');
    // Order matters: stop accepting work, drain sockets, then release clients.
    try {
      await app.close();
      await wsHub.stop();
      await riotClient.close();
      await Promise.allSettled([closeRedis(), closeDb()]);
    } catch (err) {
      logger.error({ err }, 'error during shutdown');
    } finally {
      process.exit(0);
    }
  };

  process.on('SIGTERM', () => void shutdown('SIGTERM'));
  process.on('SIGINT', () => void shutdown('SIGINT'));
}

main().catch((err: unknown) => {
  logger.error({ err }, 'api failed to start');
  process.exit(1);
});

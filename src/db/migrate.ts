import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { migrate } from 'drizzle-orm/postgres-js/migrator';
import { config } from '../config.js';
import { logger } from '../logger.js';
import { DEFAULT_QUOTA_PER_MIN } from '../quotas.js';
import { closeDb, db } from './index.js';
import { createConsumer } from './consumers.js';

const here = dirname(fileURLToPath(import.meta.url));

async function main(): Promise<void> {
  logger.info('running migrations');
  await migrate(db, { migrationsFolder: join(here, 'migrations') });
  logger.info('migrations complete');

  // Convenience for a fresh deployment: seed one admin key from env so the
  // admin surface is reachable before any key exists to create keys with.
  if (config.BOOTSTRAP_ADMIN_KEY) {
    const created = await createConsumer({
      name: 'bootstrap-admin',
      scopes: ['read', 'admin'],
      quotaPerMin: DEFAULT_QUOTA_PER_MIN,
      key: config.BOOTSTRAP_ADMIN_KEY,
      ifNotExists: true,
    });
    if (created) logger.info({ id: created.id }, 'bootstrap admin consumer created');
    else logger.info('bootstrap admin consumer already present');
  }

  await closeDb();
}

main().catch((err: unknown) => {
  logger.error({ err }, 'migration failed');
  process.exitCode = 1;
  void closeDb();
});

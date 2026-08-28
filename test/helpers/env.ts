import { config as loadDotenv } from 'dotenv';

/**
 * `src/config.ts` reads env at import time, so every test file that touches a
 * module depending on config must import this first.
 *
 * dotenv is loaded here rather than relying on config.ts's own call: the
 * fallbacks below use `??=`, which would otherwise win over a real `.env` and
 * point the suite at services that do not exist.
 */
loadDotenv({ quiet: true });

process.env['NODE_ENV'] = 'test';
process.env['RIOT_API_KEY'] ??= 'RGAPI-test-key-0000-0000-000000000000';
process.env['LOG_LEVEL'] = 'silent';
process.env['REDIS_URL'] ??= 'redis://localhost:6379';
process.env['DATABASE_URL'] ??= 'postgres://proxy:proxy@localhost:5432/riotproxy';

export {};

import { config as loadDotenv } from 'dotenv';

/**
 * `src/config.ts` reads env at import time, so every test file that touches a
 * module depending on config must import this first.
 *
 * The suite is hermetic by construction. Every setting in `EnvSchema` is pinned
 * to a known value here, so nothing a developer keeps in `.env` — or exports in
 * their shell — can change what the tests assert. Only the two connection
 * strings are allowed to vary, because those genuinely differ per machine.
 *
 * Keep this in step with `EnvSchema` in src/config.ts: a setting left out is one
 * a future test can silently inherit from the environment, and the failure then
 * points at the code rather than at the machine.
 */

// Parsed into a local object, never applied. Loading `.env` into process.env is
// precisely the leak this helper exists to prevent.
const fileEnv: Record<string, string> = {};
loadDotenv({ quiet: true, processEnv: fileEnv });

/** Machine-specific, so a real value wins: shell first, then `.env`, then this. */
export const CONNECTIONS: Record<string, string> = {
  REDIS_URL: 'redis://localhost:6379',
  DATABASE_URL: 'postgres://proxy:proxy@localhost:5432/riotproxy',
};

/**
 * Everything else, fixed regardless of the environment.
 *
 * RIOT_API_KEY is pinned rather than inherited even though it is nominally a
 * credential: every upstream call in this suite is stubbed, so a real key is
 * never used — and pinning it makes KEY_SCOPE, which namespaces every cache key
 * and stored id, identical on every machine and in CI.
 */
export const PINNED: Record<string, string> = {
  RIOT_API_KEY: 'RGAPI-test-key-0000-0000-000000000000',
  RIOT_USER_AGENT: 'riot-proxy/1.0 (+https://github.com/riot-proxy)',

  NODE_ENV: 'test',
  PORT: '8080',
  HOST: '0.0.0.0',
  LOG_LEVEL: 'silent',

  DEFAULT_PLATFORM: 'euw1',
  CACHE_TTL_OVERRIDES: '',
  NEG_TTL_SECONDS: '30',
  NEG_TTL_ACCOUNT_SECONDS: '300',
  SF_LOCK_MS: '5000',
  CLIENT_WAIT_BUDGET_MS: '2000',
  BULK_USAGE_CEILING: '0.8',
  STALE_WHILE_REVALIDATE: 'true',

  TRACK_POLL_LIVE_S: '60',
  TRACK_POLL_RANK_S: '600',
  TRACK_POLL_MATCH_S: '300',
  DDRAGON_SYNC_S: '3600',
  ARCHIVE_TIMELINES: 'false',
  // Off by default so no test accidentally queues a 10 000-match walk; the
  // tests that care about it assert the enqueue directly.
  LOOKUP_BACKFILL_LIMIT: '0',
  // Likewise: the default would let a poll page back 500 matches. The catch-up
  // tests turn it on themselves.
  TRACK_CATCHUP_LIMIT: '0',

  DDRAGON_DIR: './data/ddragon',
  DDRAGON_LOCALE: 'en_US',

  ADMIN_IP_ALLOWLIST: '',
  BOOTSTRAP_ADMIN_KEY: '',
  // The suite asserts the auth rejections themselves, so a local
  // AUTH_DISABLED=true must never bleed in and silently neuter those tests.
  AUTH_DISABLED: 'false',
  // Pinned on so the /dev surface is always exercised, and never left to
  // NODE_ENV's default.
  DEV_UI: 'true',
  // Likewise: pinned on so every app the suite builds carries the docs plugin,
  // and the document is generated on every run rather than only when someone
  // remembers to look at it.
  DOCS_UI: 'true',
};

for (const [key, fallback] of Object.entries(CONNECTIONS)) {
  process.env[key] = process.env[key] || fileEnv[key] || fallback;
}
for (const [key, value] of Object.entries(PINNED)) {
  process.env[key] = value;
}

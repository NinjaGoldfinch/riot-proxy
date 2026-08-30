import { createHash } from 'node:crypto';
import { Type, type Static } from '@sinclair/typebox';
import { Value } from '@sinclair/typebox/value';
import { config as loadDotenv } from 'dotenv';

loadDotenv();

/**
 * Env schema. Everything arrives as a string, so numeric/boolean fields are
 * declared as strings here and converted below — TypeBox `Value.Convert` does
 * the coercion, `Value.Check` enforces the constraints afterwards.
 */
const EnvSchema = Type.Object({
  RIOT_API_KEY: Type.String({ minLength: 8 }),
  RIOT_USER_AGENT: Type.String({ default: 'riot-proxy/1.0 (+https://github.com/riot-proxy)' }),

  NODE_ENV: Type.Union(
    [Type.Literal('development'), Type.Literal('test'), Type.Literal('production')],
    { default: 'development' },
  ),
  PORT: Type.Integer({ default: 8080, minimum: 1, maximum: 65535 }),
  HOST: Type.String({ default: '0.0.0.0' }),
  LOG_LEVEL: Type.String({ default: 'info' }),

  REDIS_URL: Type.String({ default: 'redis://localhost:6379' }),
  DATABASE_URL: Type.String({ default: 'postgres://proxy:proxy@localhost:5432/riotproxy' }),

  DEFAULT_PLATFORM: Type.String({ default: 'euw1' }),
  CACHE_TTL_OVERRIDES: Type.String({ default: '' }),
  NEG_TTL_SECONDS: Type.Integer({ default: 30, minimum: 1 }),
  NEG_TTL_ACCOUNT_SECONDS: Type.Integer({ default: 300, minimum: 1 }),
  SF_LOCK_MS: Type.Integer({ default: 5000, minimum: 100 }),
  CLIENT_WAIT_BUDGET_MS: Type.Integer({ default: 2000, minimum: 0 }),
  /**
   * §9.3 with the reserve set deliberately: bulk work stops taking tokens once
   * a bucket is this full, so the remainder stays free for user-invoked
   * requests. 0.80 keeps a fifth of every bucket for them — the spec's opening
   * figure was 0.75, tightened here because a first lookup now queues a full
   * history behind it and bulk has far more to do than it used to.
   */
  BULK_USAGE_CEILING: Type.Number({ default: 0.8, minimum: 0, maximum: 1 }),
  STALE_WHILE_REVALIDATE: Type.Boolean({ default: true }),

  TRACK_POLL_LIVE_S: Type.Integer({ default: 60, minimum: 10 }),
  TRACK_POLL_RANK_S: Type.Integer({ default: 600, minimum: 30 }),
  TRACK_POLL_MATCH_S: Type.Integer({ default: 300, minimum: 30 }),
  DDRAGON_SYNC_S: Type.Integer({ default: 3600, minimum: 60 }),
  ARCHIVE_TIMELINES: Type.Boolean({ default: false }),

  /**
   * How far back to walk a player's history the first time anyone looks them
   * up. 10 000 is match-v5's own `start` ceiling, so the default means "all of
   * it"; 0 turns the behaviour off and leaves backfills to the admin route.
   */
  LOOKUP_BACKFILL_LIMIT: Type.Integer({ default: 10_000, minimum: 0, maximum: 10_000 }),

  /**
   * How far back one `poll:matches` tick will page to find where it left off
   * (#46). The steady-state cost is unchanged — the cursor is on the first page
   * — so this only spends anything after the ticks themselves stopped. A gap
   * deeper than this is handed to a backfill rather than chased inline; `0`
   * restores the old fixed window and lets a deep gap go unrepaired.
   */
  TRACK_CATCHUP_LIMIT: Type.Integer({ default: 500, minimum: 0, maximum: 10_000 }),

  DDRAGON_DIR: Type.String({ default: './data/ddragon' }),
  DDRAGON_LOCALE: Type.String({ default: 'en_US' }),

  ADMIN_IP_ALLOWLIST: Type.String({ default: '' }),
  BOOTSTRAP_ADMIN_KEY: Type.String({ default: '' }),
  /**
   * Local testing escape hatch: skip consumer key checks entirely and treat
   * every request as a synthetic `dev-local` consumer with read+admin scope.
   * Refused outright when NODE_ENV=production (see `parseEnv`).
   */
  AUTH_DISABLED: Type.Boolean({ default: false }),

  /**
   * Serve the throwaway browser UI under `/dev` (see `public/dev-ui.html`).
   * Left unset it follows NODE_ENV: on everywhere but production, where a
   * debugging page that advertises whether auth is off has no business being.
   */
  DEV_UI: Type.Optional(Type.Boolean()),

  /**
   * Serve the OpenAPI document and the API reference at `/docs`.
   *
   * Unlike DEV_UI this defaults **on in production**: a published API's
   * reference is part of its contract, not a debugging tool, and one you need a
   * key to read is one nobody reads. Turning it off removes `/docs`,
   * `/openapi.json` and `/openapi.yaml` together — see the note in
   * `src/docs/plugin.ts` about why they share one flag.
   */
  DOCS_UI: Type.Boolean({ default: true }),
});

export type Env = Static<typeof EnvSchema>;

function parseEnv(source: NodeJS.ProcessEnv): Env {
  // Drop empty strings so schema defaults win over `FOO=` in a .env file.
  const raw: Record<string, unknown> = {};
  for (const key of Object.keys(EnvSchema.properties)) {
    const value = source[key];
    if (value !== undefined && value !== '') raw[key] = value;
  }

  const withDefaults = Value.Default(EnvSchema, raw);
  const converted = Value.Convert(EnvSchema, withDefaults);

  if (!Value.Check(EnvSchema, converted)) {
    const errors = [...Value.Errors(EnvSchema, converted)]
      .map((e) => `  ${e.path || '/'}: ${e.message}`)
      .join('\n');
    throw new Error(`Invalid environment configuration:\n${errors}`);
  }

  // A misplaced AUTH_DISABLED in production would silently expose the whole
  // admin surface, so fail loudly at boot instead of degrading quietly.
  if (converted.AUTH_DISABLED && converted.NODE_ENV === 'production') {
    throw new Error(
      'Invalid environment configuration:\n  AUTH_DISABLED cannot be enabled when NODE_ENV=production',
    );
  }

  return converted;
}

const env = parseEnv(process.env);

/**
 * §7.4 — encrypted Riot IDs (PUUID, summonerId) are unique per API key. Every
 * cache key and every stored encrypted ID is namespaced by this value so that
 * rotating the key invalidates stale IDs instead of silently poisoning lookups.
 */
export const KEY_SCOPE = createHash('sha256').update(env.RIOT_API_KEY).digest('hex').slice(0, 8);

/** Parse `league=120,spectator=20` into `{ league: 120, spectator: 20 }`. */
export function parseTtlOverrides(csv: string): Record<string, number> {
  const out: Record<string, number> = {};
  for (const pair of csv.split(',')) {
    const trimmed = pair.trim();
    if (!trimmed) continue;
    const eq = trimmed.indexOf('=');
    if (eq <= 0) continue;
    const key = trimmed.slice(0, eq).trim();
    const seconds = Number(trimmed.slice(eq + 1).trim());
    if (key && Number.isFinite(seconds) && seconds >= 0) out[key] = seconds;
  }
  return out;
}

export const ttlOverrides = parseTtlOverrides(env.CACHE_TTL_OVERRIDES);

export const adminIpAllowlist = env.ADMIN_IP_ALLOWLIST.split(',')
  .map((s) => s.trim())
  .filter(Boolean);

export const config = {
  ...env,
  KEY_SCOPE,
  ttlOverrides,
  adminIpAllowlist,
  authDisabled: env.AUTH_DISABLED,
  devUi: env.DEV_UI ?? env.NODE_ENV !== 'production',
  docsUi: env.DOCS_UI,
  isProduction: env.NODE_ENV === 'production',
  isTest: env.NODE_ENV === 'test',
} as const;

export type Config = typeof config;

// Exported for unit tests; the running service always uses `config`.
export const __test = { parseEnv, EnvSchema };

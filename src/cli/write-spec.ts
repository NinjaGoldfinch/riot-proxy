import { writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

/**
 * `npm run docs:spec` — writes the OpenAPI document to `openapi.json` at the
 * repo root, where CI checks it for drift (#65).
 *
 * The point of committing it is that a contract change becomes a reviewable
 * diff in the pull request that causes it, instead of something a consumer
 * discovers. It also lets client generators, Postman and contract tests read
 * the spec without booting the service.
 *
 * ## Why the environment is pinned below
 *
 * The document quotes the running configuration on purpose (§3 of the plan):
 * cache TTLs come from `ENDPOINTS`, and the reference states this deployment's
 * refresh cooldown and bulk ceiling rather than prose that used to be true.
 * That is right for the document served live at `/openapi.json`, and wrong for
 * a file under version control — a developer with `LOOKUP_BACKFILL_LIMIT=200`
 * in their `.env` would otherwise produce a legitimate-looking diff on every
 * run, and CI would fail for a reason that has nothing to do with the change.
 *
 * So the snapshot is generated against the schema defaults, and means "the
 * contract as shipped". A deployment that tunes anything still gets a document
 * that agrees with it — from its own `/openapi.json`, which is generated live.
 *
 * The pinning has to happen before `config.ts` is imported, since it reads the
 * environment once at module load. Hence the dynamic import below rather than
 * a static one at the top of the file.
 */

/**
 * Everything the document's content is derived from, at the value `EnvSchema`
 * documents as the default. Kept in step with `src/config.ts`: a new setting
 * that reaches the document and is not pinned here makes the snapshot depend on
 * whoever generated it.
 */
const CANONICAL: Record<string, string> = {
  // Never a real key. Only KEY_SCOPE derives from it, and that does not reach
  // the document.
  RIOT_API_KEY: 'RGAPI-docs-placeholder-0000-0000-000000000000',
  NODE_ENV: 'production',
  LOG_LEVEL: 'silent',
  PORT: '8080',

  NEG_TTL_SECONDS: '30',
  NEG_TTL_ACCOUNT_SECONDS: '300',
  CACHE_TTL_OVERRIDES: '',
  BULK_USAGE_CEILING: '0.8',
  LOOKUP_BACKFILL_LIMIT: '10000',

  DOCS_UI: 'true',
  DEV_UI: 'false',
  AUTH_DISABLED: 'false',
};

for (const [key, value] of Object.entries(CANONICAL)) process.env[key] = value;

const { buildApp } = await import('../app.js');
const { closeRedis } = await import('../redis.js');
const { closeDb } = await import('../db/index.js');
const { wsHub } = await import('../ws/index.js');

const OUT = fileURLToPath(new URL('../../openapi.json', import.meta.url));

const app = await buildApp();
await app.ready();

const document = app.swagger();
await writeFile(OUT, `${JSON.stringify(document, null, 2)}\n`, 'utf8');

const operations = Object.values(document.paths ?? {}).reduce(
  (n, ops) => n + Object.keys(ops as object).length,
  0,
);
process.stdout.write(`openapi.json — ${operations} operations\n`);

await app.close();
await wsHub.stop();
await Promise.allSettled([closeRedis(), closeDb()]);
process.exit(0);

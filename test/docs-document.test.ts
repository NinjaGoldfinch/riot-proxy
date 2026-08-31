import './helpers/env.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { buildApp, type App } from '../src/app.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { closeRedis, redis } from '../src/redis.js';
import { DEFAULT_STATUS, ERROR_CODES, type ErrorCode } from '../src/errors.js';
import { ERROR_EXAMPLES } from '../src/docs/examples.js';
import { EVENT_EXAMPLES } from '../src/events/examples.js';
import { ANON_QUOTA_PER_MIN, DEFAULT_QUOTA_PER_MIN } from '../src/quotas.js';
import { KEY_PREFIX } from '../src/keys.js';
import { HEARTBEAT_MS, MAX_MISSED_PONGS, MAX_TOPICS_PER_SOCKET } from '../src/ws/constants.js';
import { wsHub } from '../src/ws/index.js';

/**
 * The emitted document (#61, #62). These assert the shape of the reference
 * itself, which nothing else in the suite looks at — the failure mode this
 * guards is a document that still generates and is quietly useless.
 */
let app: App | undefined;
let doc: Record<string, any> | undefined;
let available = false;

/**
 * Every operation the document is expected to describe. Listed rather than
 * counted so a new route fails here with its own name, which is the prompt to
 * document it or hide it deliberately — the alternative is shipping an
 * endpoint the reference does not mention.
 *
 * Not here on purpose: the three `/dev` routes, which are hidden, and `/v1/ws`,
 * which never reaches the document at all because @fastify/websocket does not
 * register an operation @fastify/swagger can see. The WS protocol is documented
 * as prose on the `ws` tag instead.
 */
const EXPECTED_OPERATIONS = [
  'DELETE /v1/admin/consumers/{id}',
  'DELETE /v1/admin/tracked-players/{puuid}',
  'GET /healthz',
  'GET /metrics',
  'GET /readyz',
  'GET /v1/admin/consumers',
  'GET /v1/admin/debug/cache',
  'GET /v1/admin/debug/riot',
  'GET /v1/admin/limits/{scope}',
  'GET /v1/admin/metrics',
  'GET /v1/admin/metrics/history',
  'GET /v1/admin/stats',
  'GET /v1/admin/tracked-players',
  'GET /v1/lol/league/entries/by-puuid/{platform}/{puuid}',
  'GET /v1/lol/mastery/by-puuid/{platform}/{puuid}',
  'GET /v1/lol/matches/ids/{region}/{puuid}',
  'GET /v1/lol/matches/{region}/{matchId}',
  'GET /v1/lol/matches/{region}/{matchId}/timeline',
  'GET /v1/lol/rotations/{platform}',
  'GET /v1/lol/spectator/active/{platform}/{puuid}',
  'GET /v1/lol/status/{platform}',
  'GET /v1/lol/summoners/by-puuid/{platform}/{puuid}',
  'GET /v1/players/by-riot-id/{gameName}/{tagLine}/profile',
  'GET /v1/players/{puuid}/matches',
  'GET /v1/players/{puuid}/profile',
  'GET /v1/riot/accounts/by-puuid/{region}/{puuid}',
  'GET /v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}',
  'GET /v1/static/versions',
  'GET /v1/static/{file}',
  'POST /v1/admin/backfill',
  'POST /v1/admin/cache/purge',
  'POST /v1/admin/consumers',
  'POST /v1/admin/consumers/{id}/revoke-cache',
  'POST /v1/admin/ddragon/sync',
  'POST /v1/admin/tracked-players',
];

/**
 * Named in prose but deliberately absent from `paths`. `/v1/ws` never reaches
 * the document at all — @fastify/websocket registers no operation
 * @fastify/swagger can see — and the docs and dev routes are hidden, `/docs`
 * from itself included.
 */
const NOT_OPERATIONS = new Set(['/v1/ws', '/openapi.json', '/openapi.yaml', '/dev', '/dashboard']);
const isOperation = (path: string): boolean =>
  !NOT_OPERATIONS.has(path) && !path.startsWith('/docs');

const operations = (d: Record<string, any>): string[] =>
  Object.entries(d.paths ?? {})
    .flatMap(([path, ops]) => Object.keys(ops as object).map((m) => `${m.toUpperCase()} ${path}`))
    .sort();

beforeAll(async () => {
  available = await probeServices('docs-document.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (!available) return;
  app = await buildApp();
  await app.ready();
  doc = (app as unknown as { swagger: () => Record<string, any> }).swagger();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  await Promise.allSettled([closeRedis(), closeDb()]);
});

describe('document coverage', () => {
  it('describes every route that is not deliberately hidden', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(operations(doc)).toEqual(EXPECTED_OPERATIONS);
  });

  it('leaves nothing untagged', ({ skip }) => {
    if (!available || !doc) return skip();
    const untagged: string[] = [];
    for (const [path, ops] of Object.entries(doc.paths ?? {}))
      for (const [method, op] of Object.entries(ops as Record<string, any>))
        if (!op.tags?.length) untagged.push(`${method.toUpperCase()} ${path}`);
    expect(untagged).toEqual([]);
  });

  it('is OpenAPI 3.1', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(doc.openapi).toBe('3.1.0');
  });
});

describe('shared components (#61)', () => {
  it('registers the envelope and the parameters under their own names', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(Object.keys(doc.components?.schemas ?? {}).sort()).toEqual([
      'BackfillNotice',
      'ErrorResponse',
      'GameNameParam',
      'MatchIdParam',
      'MatchPage',
      // Extracted on its own because `match-summary.ts` gives it a `$id`: the
      // summary is the shape a caller renders, so it earns a sidebar entry.
      'MatchSummary',
      // Named so the history route's array can reference the point shape once.
      'MetricsHistoryPoint',
      // Registered because the `metrics` WS topic publishes the identical
      // document — a named schema is what lets the ws prose point at it.
      'MetricsSnapshot',
      'PlatformParam',
      'ProfileBody',
      'PuuidParam',
      'RegionParam',
      'ScopeParam',
      'TagLineParam',
    ]);
  });

  it('references the envelope instead of copying it 182 times', ({ skip }) => {
    if (!available || !doc) return skip();
    let referenced = 0;
    let inlined = 0;
    for (const ops of Object.values(doc.paths ?? {}))
      for (const op of Object.values(ops as Record<string, any>))
        for (const [status, response] of Object.entries(op.responses ?? {})) {
          if (Number(status) < 400) continue;
          const schema = (response as any).content?.['application/json']?.schema;
          if (schema?.$ref?.includes('ErrorResponse')) referenced++;
          else if (schema?.properties?.error) inlined++;
        }
    // Every one of them, and the definition still exists exactly once.
    expect(inlined).toBe(0);
    expect(referenced).toBeGreaterThan(150);
    expect(doc.components.schemas.ErrorResponse.properties.error).toBeDefined();
  });

  it('describes a PUUID once, and the routes point at it', ({ skip }) => {
    if (!available || !doc) return skip();
    const puuid = doc.components.schemas.PuuidParam;
    expect(puuid.pattern).toBe('^[A-Za-z0-9_-]+$');
    expect(puuid.description).toContain('encrypts these per API key');

    const param = doc.paths['/v1/players/{puuid}/matches'].get.parameters.find(
      (p: { name: string }) => p.name === 'puuid',
    );
    expect(param.schema.$ref).toContain('PuuidParam');
  });
});

describe('the composite bodies (#63)', () => {
  it('names the fields a caller has to write code against', ({ skip }) => {
    if (!available || !doc) return skip();
    const profile = doc.components.schemas.ProfileBody;
    expect(Object.keys(profile.properties).sort()).toEqual([
      'account',
      'ageSeconds',
      'league',
      'mastery',
      'platform',
      'puuid',
      'refreshAvailableIn',
      'refreshed',
      'region',
      'summoner',
      'warnings',
    ]);
    // ageSeconds is four named parts, not an open map — the shape
    // fast-json-stringify drops the nulls out of.
    expect(Object.keys(profile.properties.ageSeconds.properties).sort()).toEqual([
      'account',
      'league',
      'mastery',
      'summoner',
    ]);
  });

  it("leaves Riot's parts unconstrained inside our own document", ({ skip }) => {
    if (!available || !doc) return skip();
    const profile = doc.components.schemas.ProfileBody;
    // No `type`, no `properties`: an empty schema, which is what makes the
    // passthrough guarantee survive being embedded in a described body.
    for (const part of ['account', 'summoner', 'league', 'mastery']) {
      expect(profile.properties[part].type, `${part} must stay unconstrained`).toBeUndefined();
      expect(
        profile.properties[part].properties,
        `${part} must stay unconstrained`,
      ).toBeUndefined();
    }
  });

  it('points the match page at the shared backfill notice', ({ skip }) => {
    if (!available || !doc) return skip();
    const page = doc.components.schemas.MatchPage;
    expect(page.properties.hasMore.description).toContain('Page on this');
    const backfill = JSON.stringify(page.properties.backfill);
    expect(backfill).toContain('BackfillNotice');
  });
});

describe('examples (#63)', () => {
  it("shows what Riot's bodies look like without constraining them", ({ skip }) => {
    if (!available || !doc) return skip();
    const media =
      doc.paths['/v1/lol/summoners/by-puuid/{platform}/{puuid}'].get.responses['200'].content[
        'application/json'
      ];
    // An example and an empty schema: documented, not validated. If these ever
    // swap round, the proxy breaks the next time Riot adds a field.
    expect(media.example).toMatchObject({ summonerLevel: 412 });
    expect(Object.keys(media.schema ?? {})).toEqual([]);
  });

  it('uses identifiers nobody could mistake for a real account', ({ skip }) => {
    if (!available || !doc) return skip();
    const json = JSON.stringify(doc);
    const puuids = json.match(/"puuid":\s*"([^"]+)"/g) ?? [];
    expect(puuids.length).toBeGreaterThan(0);
    for (const p of puuids) expect(p).toContain('EXAMPLE-puuid-not-a-real-account');
  });

  it('tells the two throttles apart, which is the whole point', ({ skip }) => {
    if (!available || !doc) return skip();
    const responses = doc.paths['/v1/lol/status/{platform}'].get.responses;
    const quota = Object.values(responses['429'].content['application/json'].examples)[0] as any;
    const upstream = Object.values(responses['503'].content['application/json'].examples)[0] as any;

    expect(quota.value.error.code).toBe('QUOTA_EXCEEDED');
    expect(quota.value.error.retryAfter).toBeTypeOf('number');
    expect(quota.summary).toContain('Your quota');

    expect(upstream.value.error.code).toBe('RATE_LIMITED');
    expect(upstream.summary).toContain('affects everyone');
  });

  it('names only error codes that exist, in both the prose and the examples', ({ skip }) => {
    if (!available || !doc) return skip();
    // The schemas take their enum from ERROR_CODES and cannot drift. The prose
    // table and the examples are hand-written and can — this caught the
    // reference documenting an `UPSTREAM_UNAVAILABLE` that the service has
    // never been able to return.
    const documented = new Set<string>();
    for (const [, code] of (doc.info.description as string).matchAll(
      /^\| `([A-Z_]+)` \| \d{3} \|/gm,
    )) {
      if (code) documented.add(code);
    }
    expect(documented.size).toBeGreaterThan(0);
    for (const code of documented) {
      expect(ERROR_CODES, `${code} is documented but not a real error code`).toContain(code);
    }
    // And every real code is documented, so a new one cannot ship unmentioned.
    expect([...documented].sort()).toEqual([...ERROR_CODES].sort());

    for (const example of Object.values(ERROR_EXAMPLES)) {
      const code = (example.value as { error: { code: string } }).error.code;
      expect(ERROR_CODES, `${code} is used in an example but is not a real code`).toContain(code);
    }
  });

  it('states the status each code actually returns', ({ skip }) => {
    if (!available || !doc) return skip();
    // Close to tautological now that the table is generated from
    // `DEFAULT_STATUS`. It earns its place by catching the regression where
    // someone reverts the table to nine hand-written rows.
    let checked = 0;
    for (const [, code, status] of (doc.info.description as string).matchAll(
      /^\| `([A-Z_]+)` \| (\d{3}) \|/gm,
    )) {
      checked += 1;
      expect(Number(status), `${code} is documented as ${status}`).toBe(
        DEFAULT_STATUS[code as ErrorCode],
      );
    }
    expect(checked).toBe(ERROR_CODES.length);
  });

  it('does not offer an error example a route cannot produce', ({ skip }) => {
    if (!available || !doc) return skip();
    // The static mirror has no 502 response at all, so there is nothing to
    // attach a 502 example to.
    expect(doc.paths['/v1/static/{file}'].get.responses['502']).toBeUndefined();
  });

  it('carries a paging loop driven by hasMore', ({ skip }) => {
    if (!available || !doc) return skip();
    const samples = doc.paths['/v1/players/{puuid}/matches'].get['x-codeSamples'];
    expect(samples).toHaveLength(1);
    expect(samples[0].source).toContain('page.hasMore');
    expect(samples[0].source).toContain('Retry-After');
  });
});

describe('error sets are honest (#61)', () => {
  it('does not claim the static mirror can fail upstream', ({ skip }) => {
    if (!available || !doc) return skip();
    const responses = doc.paths['/v1/static/{file}'].get.responses;
    expect(Object.keys(responses).sort()).toEqual(['200', '400', '401', '403', '404', '429']);
  });

  it('keeps 429 on the local routes — the quota is ours, not Riotimposed', ({ skip }) => {
    if (!available || !doc) return skip();
    // The rate-limit plugin applies to anything not `config.public`, so a route
    // that never calls Riot can still exhaust a consumer's allowance.
    expect(doc.paths['/v1/admin/stats'].get.responses['429']).toBeDefined();
    expect(doc.paths['/v1/admin/stats'].get.responses['502']).toBeUndefined();
  });

  it('does not demand a key for health, or claim health can 401', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(doc.paths['/healthz'].get.responses['401']).toBeUndefined();
    expect(doc.paths['/healthz'].get.security).toEqual([]);
    expect(doc.paths['/metrics'].get.security).toEqual([]);
  });

  it('still claims 502 on the one admin route that reaches Riot', ({ skip }) => {
    if (!available || !doc) return skip();
    // POST /v1/admin/tracked-players resolves a Riot ID upstream when given one.
    expect(doc.paths['/v1/admin/tracked-players'].post.responses['502']).toBeDefined();
    expect(doc.paths['/v1/admin/tracked-players'].get.responses['502']).toBeUndefined();
  });
});

describe('the document itself (#62)', () => {
  it('offers localhost first so the console does not target production', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(doc.servers[0].url).toContain('localhost');
    expect(doc.servers[1].url).toBe('{scheme}://{host}');
  });

  it('documents both auth schemes, and marks the query one WS-only', ({ skip }) => {
    if (!available || !doc) return skip();
    const schemes = doc.components.securitySchemes;
    expect(schemes.bearerAuth).toMatchObject({ type: 'http', scheme: 'bearer' });
    expect(schemes.tokenQuery).toMatchObject({ type: 'apiKey', in: 'query', name: 'token' });
    expect(schemes.tokenQuery.description).toContain('WebSocket handshake only');
  });

  it('groups the tags, leading with the composites', ({ skip }) => {
    if (!available || !doc) return skip();
    expect(doc['x-tagGroups'].map((g: { name: string }) => g.name)).toEqual([
      'Player data',
      'Static data',
      'Realtime',
      'Operations',
      'Administration',
    ]);
    expect(doc['x-tagGroups'][0].tags[0]).toBe('players');
  });

  it('documents the WebSocket protocol as prose, since 3.1 cannot express it', ({ skip }) => {
    if (!available || !doc) return skip();
    const ws = doc.tags.find((t: { name: string }) => t.name === 'ws');
    expect(ws.description).toContain('?token=');
    // Iterated rather than listed: the samples are the source the tag is
    // rendered from, so a renamed field fails here as well as failing to
    // compile — and a new event cannot ship undocumented.
    for (const [event, sample] of Object.entries(EVENT_EXAMPLES)) {
      expect(ws.description, `${event} missing from the ws tag`).toContain(event);
      for (const field of Object.keys(sample.data)) {
        expect(ws.description, `${event}.${field} missing from the ws tag`).toContain(`"${field}"`);
      }
    }
    expect(ws.description).toContain('only ever fires for a _tracked_ player');
  });

  it('states the socket limits the hub enforces, and the topic ceiling', ({ skip }) => {
    if (!available || !doc) return skip();
    const ws = doc.tags.find((t: { name: string }) => t.name === 'ws');
    expect(ws.description).toContain(`pings every ${HEARTBEAT_MS / 1000} s`);
    expect(ws.description).toContain(`misses ${MAX_MISSED_PONGS} consecutive pongs`);
    // The 200-topic cap is silent at runtime — a client that subscribes past it
    // is acknowledged and simply not subscribed — so the reference is the only
    // place it can be discovered.
    expect(ws.description).toContain(`at most ${MAX_TOPICS_PER_SOCKET} topics`);
  });

  it('generates its numbers from config rather than stating them', ({ skip }) => {
    if (!available || !doc) return skip();
    const d = doc.info.description as string;
    // One row per method in ENDPOINTS, not a hand-written table.
    expect((d.match(/^\| `[a-z]/gm) ?? []).length).toBe(12);
    expect(d).toContain('`account.byRiotId`');
    // BULK_USAGE_CEILING 0.8, REFRESH_COOLDOWN_S 60, LOOKUP_BACKFILL_LIMIT off
    // in the test env — each read, not typed.
    expect(d).toMatch(/bucket is\s+80 % full/);
    expect(d).toMatch(/one refresh per player per\s+60 s/);
    // LOOKUP_BACKFILL_LIMIT is 0 in the test env, and the prose says so rather
    // than advertising a walk that will not happen.
    expect(d).toContain('(disabled on this deployment)');
  });

  it('quotes the quotas the service enforces, not numbers typed here', ({ skip }) => {
    if (!available || !doc) return skip();
    // Prettier wraps the template literal, so a sentence is not a line.
    const prose = (doc.info.description as string).replace(/\s+/g, ' ');
    expect(prose).toContain(`the default is ${DEFAULT_QUOTA_PER_MIN}/min`);
    expect(prose).toContain(`unauthenticated callers get ${ANON_QUOTA_PER_MIN}/min`);
    expect(prose).toContain(`Authorization: Bearer ${KEY_PREFIX}`);
  });

  it('names only routes that exist', ({ skip }) => {
    if (!available || !doc) return skip();
    // The prose names routes by path, and the prose and the route table are
    // generated from different things — so a rename leaves a dead pointer in
    // the document a consumer is reading to find that exact route, and nothing
    // else in the suite would notice.
    const prose = [
      doc.info.description as string,
      ...(doc.tags as { description?: string }[]).map((t) => t.description ?? ''),
      ...Object.values(
        doc.components.securitySchemes as Record<string, { description?: string }>,
      ).map((s) => s.description ?? ''),
    ].join('\n\n');

    const routed = new Set(Object.keys(doc.paths));
    let checked = 0;

    // Both forms the prose uses: `POST /v1/admin/consumers` when the reader is
    // being told to call something, and a bare `/readyz` when it is being
    // pointed at. The method is optional and the trailing `[^`]*` is what lets
    // `/v1/ws?token=…` match with its query string while capturing only the
    // path — without it that one is skipped in silence and the exemption above
    // is never exercised.
    for (const [, path] of prose.matchAll(
      /`(?:(?:GET|POST|PATCH|PUT|DELETE) )?(\/[A-Za-z0-9/_{}.-]*)[^`]*`/g,
    )) {
      if (!isOperation(path!)) continue;
      checked += 1;
      expect(routed, `the prose names ${path}, which is not a route`).toContain(path!);
    }

    // A regex over prose passes vacuously when it matches nothing, which is the
    // failure this guards against: reformat one line and the assertion quietly
    // stops running.
    expect(checked).toBeGreaterThan(2);
  });

  it('badges each route with what it costs', ({ skip }) => {
    if (!available || !doc) return skip();
    const badge = (path: string, method = 'get') =>
      (doc!.paths[path][method]['x-badges'] ?? []).map((b: { name: string }) => b.name);

    // Read from ENDPOINTS: account-v1 is 86 400 s, a match is immutable.
    expect(badge('/v1/riot/accounts/by-puuid/{region}/{puuid}')).toContain('Cache 1 d');
    expect(badge('/v1/lol/matches/{region}/{matchId}')).toContain('Immutable · archived');
    expect(badge('/v1/static/versions')).toContain('No upstream call');
    expect(badge('/v1/players/{puuid}/matches')).toContain('Composite');
    expect(badge('/healthz')).toContain('Public · no key');
    expect(badge('/v1/admin/stats')).toContain('Admin scope');
  });
});

describe('serving the reference (#64)', () => {
  it('serves the page and both document formats', async ({ skip }) => {
    if (!available || !app) return skip();
    const page = await app.inject({ method: 'GET', url: '/docs/' });
    expect(page.statusCode).toBe(200);
    expect(page.body).toMatch(/<script|<html/i);

    const json = await app.inject({ method: 'GET', url: '/openapi.json' });
    expect(json.statusCode).toBe(200);
    expect(json.json().openapi).toBe('3.1.0');

    const yaml = await app.inject({ method: 'GET', url: '/openapi.yaml' });
    expect(yaml.statusCode).toBe(200);
    expect(yaml.headers['content-type']).toContain('yaml');
    expect(yaml.body).toContain('openapi: 3.1.0');
  });

  it('serves the renderer locally rather than from a CDN', async ({ skip }) => {
    if (!available || !app) return skip();
    const bundle = await app.inject({ method: 'GET', url: '/docs/js/scalar.js' });
    expect(bundle.statusCode).toBe(200);
    expect(bundle.body.length).toBeGreaterThan(1_000_000);
  });

  it('needs no key and spends no quota — the suite runs with auth on', async ({ skip }) => {
    if (!available || !app) return skip();
    for (const url of ['/docs/', '/docs/js/scalar.js', '/openapi.json']) {
      const res = await app.inject({ method: 'GET', url });
      expect(res.statusCode, `${url} should not require a key`).toBe(200);
      // Absent rate-limit headers mean the limiter's allowList exempted it; a
      // 3.7 MB bundle must not cost the reader 60 requests of their allowance.
      expect(res.headers['x-ratelimit-limit'], `${url} should be quota-exempt`).toBeUndefined();
    }
  });
});

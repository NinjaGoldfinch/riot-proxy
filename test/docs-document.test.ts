import './helpers/env.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { buildApp, type App } from '../src/app.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { closeRedis, redis } from '../src/redis.js';
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

const operations = (d: Record<string, any>): string[] =>
  Object.entries(d.paths ?? {})
    .flatMap(([path, ops]) => Object.keys(ops as object).map((m) => `${m.toUpperCase()} ${path}`))
    .sort();

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
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
      'ErrorResponse',
      'GameNameParam',
      'MatchIdParam',
      'PlatformParam',
      'PuuidParam',
      'RegionParam',
      'ScopeParam',
      'TagLineParam',
    ]);
  });

  it('references the envelope instead of copying it 182 times', ({ skip }) => {
    if (!available || !doc) return skip();
    // The envelope's distinctive field appears once: in the component itself.
    const copies = (JSON.stringify(doc).match(/"retryAfter"/g) ?? []).length;
    expect(copies).toBe(1);

    const err = doc.paths['/v1/lol/status/{platform}'].get.responses['429'];
    expect(err.content['application/json'].schema.$ref).toContain('ErrorResponse');
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
    for (const event of [
      'game.started',
      'game.ended',
      'rank.changed',
      'match.archived',
      'patch.new',
    ]) {
      expect(ws.description, `${event} missing from the ws tag`).toContain(event);
    }
    expect(ws.description).toContain('only ever fires for a _tracked_ player');
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

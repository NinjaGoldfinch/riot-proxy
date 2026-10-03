import { readFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';

/**
 * A scripted Riot for the acceptance suite (plan P8-01, ADR-059). One address
 * stands in for every Riot host: v2 sends the host it meant in `x-riot-host`
 * when its base URL is overridden, which is how this keeps Riot's buckets per
 * host and can tell where a request was routed.
 *
 * The world it serves is small and fixed:
 *  - the player (`Acceptance#MOCK`) with MATCHES ranked games, newest first;
 *  - a second player (`Other#MOCK`), for "events of other topics" checks;
 *  - a MASTER ladder of LADDER players who share those games;
 *  - spectator state the tests flip through `POST /__mock/state`;
 *  - Data Dragon and the queue table.
 *
 * Rate limits are Riot's model: per host an app window set, per (host, method)
 * a method window set, `X-*-Rate-Limit[-Count]` headers on every answer, and an
 * accountable 429 (`X-Rate-Limit-Type: application|method`, `Retry-After`) when
 * a request would overflow a window. A proxy that paces itself never sees one.
 */

export const PLATFORM = 'oc1';
export const MATCHES = 60;
export const LADDER = 30;
export const APP_LIMITS: [number, number][] = [
  [50, 1],
  [3000, 60],
];
export const METHOD_LIMITS: [number, number][] = [[200, 10]];

const pad = (prefix: string) => (prefix + '-').padEnd(78, 'x');
export const PLAYER = { gameName: 'Acceptance', tagLine: 'MOCK', puuid: pad('acceptance-player') };
export const OTHER = { gameName: 'Other', tagLine: 'MOCK', puuid: pad('acceptance-other') };
export const ladderPuuid = (i: number) => pad(`acceptance-ladder-${String(i).padStart(2, '0')}`);

export const matchId = (k: number) => `OC1_${700000 + k}`;
/** Newest first: match 0 is the most recent. */
const END = Date.UTC(2026, 9, 1);
const TEMPLATE = JSON.parse(readFileSync(new URL('../fixtures/match.json', import.meta.url), 'utf8'));

/** Match `k`'s players: the player, and nine of the ladder in a sliding window. */
export function playersOf(k: number): string[] {
  return [PLAYER.puuid, ...Array.from({ length: 9 }, (_, j) => ladderPuuid((k * 3 + j) % LADDER))];
}

function matchBody(k: number): unknown {
  const body = structuredClone(TEMPLATE);
  const players = playersOf(k);
  body.metadata.matchId = matchId(k);
  body.metadata.participants = players;
  body.info.gameId = 700000 + k;
  body.info.platformId = 'OC1';
  // A game finished during the run is newer than every scripted one.
  body.info.gameEndTimestamp = k < MATCHES ? END - k * 3_600_000 : Date.now() - 60_000;
  body.info.gameStartTimestamp = body.info.gameEndTimestamp - 1_800_000;
  body.info.participants.forEach((p: Record<string, unknown>, slot: number) => {
    p['puuid'] = players[slot];
    p['riotIdGameName'] = slot === 0 ? PLAYER.gameName : `Ladder${slot}`;
    p['riotIdTagline'] = 'MOCK';
  });
  return body;
}

interface State {
  /** puuid → gameId while in a game. */
  inGame: Map<string, number>;
  /** Requests by `host path`, for tests that count. */
  requests: Map<string, number>;
  /** Accountable 429s this mock answered (should stay 0). */
  rejected: number;
  /** Matches added when a game ended, newest last (`k` ≥ MATCHES). */
  finished: number[];
}

const state: State = { inGame: new Map(), requests: new Map(), rejected: 0, finished: [] };

// ── Rate limits ──────────────────────────────────────────────────────────────

const stamps = new Map<string, number[]>();

/**
 * Transport jitter forgiven at a window's edge. The proxy's limiter keeps no
 * more than `limit` admissions in any rolling window *as it admits them*; the
 * requests reach this server a few milliseconds apart from those instants, so
 * one admitted exactly a window after another can arrive just inside it. Real
 * Riot is not judged at millisecond precision either. 50 ms is far below any
 * real drift: a limiter that overspent a window would still be caught.
 */
const JITTER_MS = 50;

/** Requests in `key` inside the last `seconds` (less the jitter allowance). */
function used(key: string, seconds: number, now: number): number {
  return (stamps.get(key) ?? []).filter((t) => t > now - seconds * 1000 + JITTER_MS).length;
}

function admit(host: string, method: string): { ok: boolean; type?: string; headers: Record<string, string> } {
  const now = Date.now();
  const appKey = `app ${host}`;
  const methodKey = `method ${host} ${method}`;
  const over = (key: string, limits: [number, number][]) =>
    limits.find(([limit, seconds]) => used(key, seconds, now) + 1 > limit);
  const appOver = over(appKey, APP_LIMITS);
  const methodOver = over(methodKey, METHOD_LIMITS);
  if (!appOver && !methodOver) {
    for (const key of [appKey, methodKey]) {
      const list = (stamps.get(key) ?? []).filter((t) => t > now - 60_000);
      list.push(now);
      stamps.set(key, list);
    }
  }
  const fmt = (limits: [number, number][]) => limits.map(([l, s]) => `${l}:${s}`).join(',');
  const counts = (key: string, limits: [number, number][]) =>
    limits.map(([, s]) => `${used(key, s, now)}:${s}`).join(',');
  const headers: Record<string, string> = {
    'x-app-rate-limit': fmt(APP_LIMITS),
    'x-app-rate-limit-count': counts(appKey, APP_LIMITS),
    'x-method-rate-limit': fmt(METHOD_LIMITS),
    'x-method-rate-limit-count': counts(methodKey, METHOD_LIMITS),
  };
  if (appOver || methodOver) {
    headers['x-rate-limit-type'] = appOver ? 'application' : 'method';
    headers['retry-after'] = String((appOver ?? methodOver)![1]);
    return { ok: false, type: headers['x-rate-limit-type'], headers };
  }
  return { ok: true, headers };
}

// ── Routes ───────────────────────────────────────────────────────────────────

type Answer = { status: number; body: unknown; method: string } | undefined;

const notFound = (method: string): Answer => ({
  status: 404,
  body: { status: { message: 'Data not found', status_code: 404 } },
  method,
});
const ok = (body: unknown, method: string): Answer => ({ status: 200, body, method });

const players = [PLAYER, OTHER];
const byPuuid = (puuid: string) =>
  players.find((p) => p.puuid === puuid) ??
  (puuid.startsWith('acceptance-ladder-') ? { gameName: null, tagLine: null, puuid } : undefined);

function idsFor(puuid: string): string[] {
  const newest = [...state.finished].reverse();
  const ks = [...newest, ...Array.from({ length: MATCHES }, (_, k) => k)].filter((k) =>
    playersOf(k).includes(puuid),
  );
  return ks.map(matchId);
}

function riot(path: string, query: URLSearchParams): Answer {
  let m: RegExpMatchArray | null;
  if ((m = path.match(/^\/riot\/account\/v1\/accounts\/by-riot-id\/([^/]+)\/([^/]+)$/))) {
    const [name, tag] = [decodeURIComponent(m[1]!), decodeURIComponent(m[2]!)];
    const p = players.find(
      (x) => x.gameName.toLowerCase() === name.toLowerCase() && x.tagLine.toLowerCase() === tag.toLowerCase(),
    );
    return p ? ok(p, 'account.byRiotId') : notFound('account.byRiotId');
  }
  if ((m = path.match(/^\/riot\/account\/v1\/accounts\/by-puuid\/([^/]+)$/))) {
    const p = byPuuid(m[1]!);
    return p ? ok(p, 'account.byPuuid') : notFound('account.byPuuid');
  }
  if ((m = path.match(/^\/lol\/summoner\/v4\/summoners\/by-puuid\/([^/]+)$/))) {
    return byPuuid(m[1]!)
      ? ok({ puuid: m[1], profileIconId: 29, revisionDate: END, summonerLevel: 321 }, 'summoner.byPuuid')
      : notFound('summoner.byPuuid');
  }
  if ((m = path.match(/^\/lol\/league\/v4\/entries\/by-puuid\/([^/]+)$/))) {
    return ok([], 'league.entriesByPuuid');
  }
  if ((m = path.match(/^\/lol\/league\/v4\/(challenger|grandmaster|master)leagues\/by-queue\/([^/]+)$/))) {
    const tier = m[1]!.toUpperCase();
    const entries =
      tier === 'MASTER'
        ? Array.from({ length: LADDER }, (_, i) => ({
            puuid: ladderPuuid(i),
            leaguePoints: 900 - i * 7,
            rank: 'I',
            wins: 120,
            losses: 100,
            veteran: false,
            inactive: false,
            freshBlood: false,
            hotStreak: i % 5 === 0,
          }))
        : [];
    return ok({ tier, leagueId: `mock-${tier}`, queue: m[2], name: 'Mock League', entries }, `league.${m[1]}`);
  }
  if ((m = path.match(/^\/lol\/league\/v4\/entries\/([^/]+)\/([^/]+)\/([^/]+)$/))) {
    return ok([], 'league.entriesByTier');
  }
  if ((m = path.match(/^\/lol\/match\/v5\/matches\/by-puuid\/([^/]+)\/ids$/))) {
    if (!byPuuid(m[1]!)) return notFound('match.idsByPuuid');
    const start = Number(query.get('start') ?? 0);
    const count = Number(query.get('count') ?? 20);
    const queue = query.get('queue');
    const ids = queue && queue !== '420' ? [] : idsFor(m[1]!);
    return ok(ids.slice(start, start + count), 'match.idsByPuuid');
  }
  if ((m = path.match(/^\/lol\/match\/v5\/matches\/(OC1_\d+)\/timeline$/))) {
    return ok({ metadata: { matchId: m[1] }, info: { frames: [] } }, 'match.timeline');
  }
  if ((m = path.match(/^\/lol\/match\/v5\/matches\/(OC1_(\d+))$/))) {
    const k = Number(m[2]) - 700000;
    const known = (k >= 0 && k < MATCHES) || state.finished.includes(k);
    return known ? ok(matchBody(k), 'match.byId') : notFound('match.byId');
  }
  if ((m = path.match(/^\/lol\/spectator\/v5\/active-games\/by-summoner\/([^/]+)$/))) {
    const gameId = state.inGame.get(m[1]!);
    return gameId === undefined
      ? notFound('spectator.activeGame')
      : ok(
          {
            gameId,
            gameQueueConfigId: 420,
            gameMode: 'CLASSIC',
            platformId: 'OC1',
            participants: [{ puuid: m[1], championId: 134, teamId: 100 }],
          },
          'spectator.activeGame',
        );
  }
  if (path.startsWith('/lol/champion-mastery/v4/champion-masteries/by-puuid/')) {
    return ok([{ championId: 134, championLevel: 7, championPoints: 123456 }], 'mastery');
  }
  if (path === '/lol/platform/v3/champion-rotations') {
    return ok({ freeChampionIds: [1, 2, 3], freeChampionIdsForNewPlayers: [1], maxNewPlayerLevel: 10 }, 'rotations');
  }
  if (path === '/lol/status/v4/platform-data') {
    return ok({ id: 'OC1', name: 'Oceania', locales: ['en_AU'], maintenances: [], incidents: [] }, 'status');
  }
  return undefined;
}

/** Data Dragon and the queue table: no key, no rate limit. */
function ddragon(path: string): unknown {
  if (path === '/api/versions.json') return ['16.19.1', '16.18.1'];
  if (path === '/docs/lol/queues.json')
    return [
      { queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: null },
      { queueId: 450, map: 'Howling Abyss', description: '5v5 ARAM games', notes: null },
    ];
  const m = path.match(/^\/cdn\/([\d.]+)\/data\/en_US\/(\w+)\.json$/);
  if (!m) return undefined;
  if (m[2] === 'champion')
    return {
      type: 'champion',
      version: m[1],
      data: Object.fromEntries(
        TEMPLATE.info.participants.map((p: { championId: number; championName: string }) => [
          p.championName,
          { key: String(p.championId), name: p.championName },
        ]),
      ),
    };
  return { type: m[2], version: m[1], data: {} };
}

function send(res: ServerResponse, status: number, body: unknown, headers: Record<string, string> = {}) {
  res.writeHead(status, { 'content-type': 'application/json;charset=utf-8', ...headers });
  res.end(body === undefined ? '' : JSON.stringify(body));
}

async function readBody(req: IncomingMessage): Promise<string> {
  let text = '';
  for await (const chunk of req) text += chunk;
  return text;
}

async function handle(req: IncomingMessage, res: ServerResponse) {
  const url = new URL(req.url ?? '/', 'http://mock');
  const path = url.pathname;

  // ── control ────────────────────────────────────────────────────────────────
  if (path === '/__mock/state' && req.method === 'POST') {
    const { inGame } = JSON.parse((await readBody(req)) || '{}') as { inGame?: Record<string, number | null> };
    for (const [puuid, game] of Object.entries(inGame ?? {})) {
      if (game === null) {
        // The game ends: its match joins the player's history, as Riot's would.
        if (state.inGame.delete(puuid)) state.finished.push(MATCHES + state.finished.length);
      } else state.inGame.set(puuid, game);
    }
    return send(res, 200, { ok: true });
  }
  if (path === '/__mock/requests') {
    return send(res, 200, { requests: Object.fromEntries(state.requests), rejected: state.rejected });
  }

  const dd = ddragon(path);
  if (dd !== undefined) return send(res, 200, dd);

  if (!req.headers['x-riot-token']) return send(res, 401, { status: { message: 'Unauthorized', status_code: 401 } });
  const host = String(req.headers['x-riot-host'] ?? 'unknown');
  const answer = riot(path, url.searchParams) ?? notFound('unknown');
  const key = `${host} ${path}`;
  state.requests.set(key, (state.requests.get(key) ?? 0) + 1);
  const limit = admit(host, answer!.method);
  if (!limit.ok) {
    state.rejected += 1;
    return send(res, 429, { status: { message: 'Rate limit exceeded', status_code: 429 } }, limit.headers);
  }
  return send(res, answer!.status, answer!.body, limit.headers);
}

export interface MockRiot {
  url: string;
  close(): Promise<void>;
}

export async function startMockRiot(port = 0): Promise<MockRiot> {
  const server: Server = createServer((req, res) => {
    handle(req, res).catch((err) => send(res, 500, { error: String(err) }));
  });
  await new Promise<void>((resolve) => server.listen(port, '127.0.0.1', resolve));
  const { port: bound } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${bound}`,
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}

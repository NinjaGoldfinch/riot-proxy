import { config as loadDotenv } from 'dotenv';
import { OTHER, PLATFORM, PLAYER } from './mock-riot.js';

// Live mode only: the repo's `.env` holds a real key, which mock mode must not
// read, let alone hand to a process.
if (process.env['ACCEPTANCE_LIVE'] === '1') {
  loadDotenv({ path: new URL('../../.env', import.meta.url).pathname, quiet: true });
}

/**
 * Two modes (owner decision at P8-01, ADR-059):
 *
 * - **mock** (default; `just acceptance` and CI): the suite starts a scripted
 *   Riot (`mock-riot.ts`) and `riot-proxy serve` pointed at it, with a known
 *   admin key. Every check runs, including the crawl and the live-game ones,
 *   because the mock can put a player in a game.
 * - **live** (`ACCEPTANCE_LIVE=1`): v1's mode. A real key and a Riot ID to
 *   point it at, against a server that is running or started here from `.env`.
 *   Run by hand only; it spends the key's quota.
 */
export type Mode = 'mock' | 'live';

export interface AcceptanceConfig {
  mode: Mode;
  baseUrl: string;
  wsUrl: string;
  apiKey: string | undefined;
  gameName: string;
  tagLine: string;
  /** A second tracked player (mock mode only). */
  other: { gameName: string; tagLine: string; puuid: string } | undefined;
  platform: string;
  /** match-v5 host for the platform — `sea` for OCE, and legitimately so. */
  region: string;
  phase2Requests: number;
  backfillLimit: number;
  pollLiveSeconds: number;
  liveGame: boolean;
  ladder: boolean;
  /** The mock's address, for tests that drive it. */
  mockUrl: string | undefined;
  port: number;
  mockPort: number;
}

/** routing.rs `Platform::region` (v1 `platformToRegion`). */
const REGION: Record<string, string> = {
  br1: 'americas', la1: 'americas', la2: 'americas', na1: 'americas',
  eun1: 'europe', euw1: 'europe', ru: 'europe', tr1: 'europe',
  jp1: 'asia', kr: 'asia',
  oc1: 'sea', ph2: 'sea', sg2: 'sea', th2: 'sea', tw2: 'sea', vn2: 'sea',
};

/** The admin key `serve` is started with in mock mode (BOOTSTRAP_ADMIN_KEY). */
export const MOCK_ADMIN_KEY = 'rpx_acceptance-admin-key-not-a-secret';

/** A key that is obviously a stand-in should disable live mode, not fail it. */
function keyLooksReal(key: string | undefined): key is string {
  if (!key) return false;
  if (!key.startsWith('RGAPI-')) return false;
  return !/placeholder|test-key|0000-0000/i.test(key);
}

function parseRiotId(raw: string): { gameName: string; tagLine: string } | undefined {
  const hash = raw.lastIndexOf('#');
  if (hash <= 0 || hash === raw.length - 1) return undefined;
  return { gameName: raw.slice(0, hash), tagLine: raw.slice(hash + 1) };
}

function num(name: string, fallback: number): number {
  const parsed = Number(process.env[name]);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

export interface Disabled {
  enabled: false;
  reason: string;
}

export type Resolved = ({ enabled: true } & AcceptanceConfig) | Disabled;

export function resolveConfig(): Resolved {
  const live = process.env['ACCEPTANCE_LIVE'] === '1';
  const port = num('ACCEPTANCE_PORT', 18980);
  const mockPort = num('ACCEPTANCE_MOCK_PORT', 18981);
  const baseUrl = process.env['ACCEPTANCE_BASE_URL'] ?? `http://127.0.0.1:${port}`;
  const common = {
    baseUrl,
    wsUrl: `${baseUrl.replace(/^http/, 'ws')}/v1/ws`,
    port,
    mockPort,
    phase2Requests: num('ACCEPTANCE_PHASE2_REQUESTS', 60),
    backfillLimit: num('ACCEPTANCE_BACKFILL_LIMIT', 40),
  };

  if (!live) {
    return {
      enabled: true,
      mode: 'mock',
      ...common,
      apiKey: MOCK_ADMIN_KEY,
      gameName: PLAYER.gameName,
      tagLine: PLAYER.tagLine,
      other: OTHER,
      platform: PLATFORM,
      region: REGION[PLATFORM]!,
      // TRACK_POLL_LIVE_S's floor; setup starts serve with it.
      pollLiveSeconds: 10,
      liveGame: true,
      ladder: true,
      mockUrl: `http://127.0.0.1:${mockPort}`,
    };
  }

  const key = process.env['RIOT_API_KEY'];
  if (!keyLooksReal(key)) {
    return { enabled: false, reason: 'ACCEPTANCE_LIVE=1 needs a real RIOT_API_KEY' };
  }
  const rawId = process.env['ACCEPTANCE_RIOT_ID'];
  if (!rawId) {
    return { enabled: false, reason: 'ACCEPTANCE_RIOT_ID is unset — set it to e.g. "NinjaGoldfinch#OCENZ"' };
  }
  const riotId = parseRiotId(rawId);
  if (!riotId) {
    return { enabled: false, reason: `ACCEPTANCE_RIOT_ID "${rawId}" is not in Name#TAG form` };
  }
  const platform = (process.env['ACCEPTANCE_PLATFORM'] ?? 'oc1').toLowerCase();
  const region = REGION[platform];
  if (!region) return { enabled: false, reason: `ACCEPTANCE_PLATFORM "${platform}" is not a platform` };

  return {
    enabled: true,
    mode: 'live',
    ...common,
    apiKey: process.env['ACCEPTANCE_API_KEY'],
    ...riotId,
    other: undefined,
    platform,
    region,
    pollLiveSeconds: num('TRACK_POLL_LIVE_S', 60),
    liveGame: process.env['ACCEPTANCE_LIVE_GAME'] === '1',
    ladder: process.env['ACCEPTANCE_LADDER'] === '1',
    mockUrl: undefined,
  };
}

export const acceptance = resolveConfig();

/** Narrowed accessor — every test guards on `acceptance.enabled` first. */
export function cfg(): AcceptanceConfig {
  if (!acceptance.enabled) throw new Error(`acceptance suite disabled: ${acceptance.reason}`);
  return acceptance;
}

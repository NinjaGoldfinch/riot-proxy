import { config as loadDotenv } from 'dotenv';
import { platformToRegion, type Platform } from '../../src/riot/routing.js';

loadDotenv({ quiet: true });

/**
 * The acceptance suite is the only thing here that talks to the real Riot API,
 * so it is opt-in twice over: a key that looks real, and a Riot ID to point it
 * at. Everything else has a working default.
 */
export interface AcceptanceConfig {
  baseUrl: string;
  wsUrl: string;
  apiKey: string | undefined;
  gameName: string;
  tagLine: string;
  platform: Platform;
  /** match-v5 host for the platform — `sea` for OCE, and legitimately so. */
  region: string;
  phase2Requests: number;
  backfillLimit: number;
  pollLiveSeconds: number;
  liveGame: boolean;
  redisUrl: string;
}

/** A key that is obviously a stand-in should disable the suite, not fail it. */
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
  const key = process.env['RIOT_API_KEY'];
  if (!keyLooksReal(key)) {
    return {
      enabled: false,
      reason: 'RIOT_API_KEY is unset or a placeholder — these checks need a real Riot key',
    };
  }

  const rawId = process.env['ACCEPTANCE_RIOT_ID'];
  if (!rawId) {
    return {
      enabled: false,
      reason: 'ACCEPTANCE_RIOT_ID is unset — set it to e.g. "NinjaGoldfinch#OCENZ"',
    };
  }
  const riotId = parseRiotId(rawId);
  if (!riotId) {
    return { enabled: false, reason: `ACCEPTANCE_RIOT_ID "${rawId}" is not in Name#TAG form` };
  }

  const platform = (process.env['ACCEPTANCE_PLATFORM'] ?? 'oc1').toLowerCase() as Platform;
  const port = process.env['PORT'] ?? '8080';
  const baseUrl = process.env['ACCEPTANCE_BASE_URL'] ?? `http://127.0.0.1:${port}`;

  return {
    enabled: true,
    baseUrl,
    wsUrl: `${baseUrl.replace(/^http/, 'ws')}/v1/ws`,
    apiKey: process.env['ACCEPTANCE_API_KEY'],
    ...riotId,
    platform,
    region: platformToRegion(platform),
    // 500 is what the spec asks for and takes ~10 min on a dev key's 100/2min
    // bucket. The default is small enough to run on every scheduled build.
    phase2Requests: num('ACCEPTANCE_PHASE2_REQUESTS', 60),
    backfillLimit: num('ACCEPTANCE_BACKFILL_LIMIT', 40),
    pollLiveSeconds: num('TRACK_POLL_LIVE_S', 60),
    liveGame: process.env['ACCEPTANCE_LIVE_GAME'] === '1',
    redisUrl: process.env['REDIS_URL'] ?? 'redis://localhost:6379',
  };
}

export const acceptance = resolveConfig();

/** Narrowed accessor — every test guards on `acceptance.enabled` first. */
export function cfg(): AcceptanceConfig {
  if (!acceptance.enabled) throw new Error(`acceptance suite disabled: ${acceptance.reason}`);
  return acceptance;
}

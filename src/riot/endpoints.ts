import { config } from '../config.js';
import { platformHost, regionHost, type Platform, type Region } from './routing.js';

/**
 * §5.3 + §8.2 as code. A "method id" is Riot's rate-limit method granularity —
 * the limiter buckets on it (§9.2) and the cache key embeds it (§8.1), so the
 * ids must stay stable even if the URL shape changes.
 */
export const METHOD_IDS = [
  'account.byRiotId',
  'account.byPuuid',
  'summoner.byPuuid',
  'league.entriesByPuuid',
  'match.idsByPuuid',
  'match.byId',
  'match.timeline',
  'spectator.activeGame',
  'mastery.byPuuid',
  'mastery.topByPuuid',
  'platform.championRotations',
  'status.platformData',
] as const;

export type MethodId = (typeof METHOD_IDS)[number];

/** Which host family a method routes to (§5.1 routing rule). */
export type HostKind = 'regional' | 'platform';

export interface EndpointSpec {
  id: MethodId;
  host: HostKind;
  /** Cache TTL in seconds. `Infinity` means "immutable, archive forever" (§8.2). */
  ttlSeconds: number;
  /** Negative-cache TTL for upstream 404s (§8.3). 0 disables negative caching. */
  negTtlSeconds: number;
  /** Immutable payloads are archived in Postgres rather than expiring (§7.3). */
  immutable: boolean;
  /** Short key used by `CACHE_TTL_OVERRIDES` (§14). */
  overrideKey: string;
}

const SPECS: Record<MethodId, Omit<EndpointSpec, 'id'>> = {
  'account.byRiotId': {
    host: 'regional',
    ttlSeconds: 86_400,
    negTtlSeconds: config.NEG_TTL_ACCOUNT_SECONDS,
    immutable: false,
    overrideKey: 'account',
  },
  'account.byPuuid': {
    host: 'regional',
    ttlSeconds: 86_400,
    negTtlSeconds: config.NEG_TTL_ACCOUNT_SECONDS,
    immutable: false,
    overrideKey: 'account',
  },
  'summoner.byPuuid': {
    host: 'platform',
    ttlSeconds: 3600,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'summoner',
  },
  'league.entriesByPuuid': {
    host: 'platform',
    ttlSeconds: 300,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'league',
  },
  'match.idsByPuuid': {
    host: 'regional',
    ttlSeconds: 120,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'matchIds',
  },
  'match.byId': {
    host: 'regional',
    ttlSeconds: Infinity,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: true,
    overrideKey: 'match',
  },
  'match.timeline': {
    host: 'regional',
    ttlSeconds: Infinity,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: true,
    overrideKey: 'timeline',
  },
  'spectator.activeGame': {
    host: 'platform',
    ttlSeconds: 30,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'spectator',
  },
  'mastery.byPuuid': {
    host: 'platform',
    ttlSeconds: 3600,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'mastery',
  },
  'mastery.topByPuuid': {
    host: 'platform',
    ttlSeconds: 3600,
    negTtlSeconds: config.NEG_TTL_SECONDS,
    immutable: false,
    overrideKey: 'mastery',
  },
  'platform.championRotations': {
    host: 'platform',
    ttlSeconds: 21_600,
    negTtlSeconds: 0,
    immutable: false,
    overrideKey: 'rotations',
  },
  'status.platformData': {
    host: 'platform',
    ttlSeconds: 60,
    negTtlSeconds: 0,
    immutable: false,
    overrideKey: 'status',
  },
};

export function endpoint(id: MethodId): EndpointSpec {
  const spec = SPECS[id];
  const override = config.ttlOverrides[spec.overrideKey];
  return { id, ...spec, ttlSeconds: override ?? spec.ttlSeconds };
}

export const ENDPOINTS: EndpointSpec[] = METHOD_IDS.map(endpoint);

export interface BuiltRequest {
  method: MethodId;
  host: string;
  path: string;
  query: Record<string, string | number | undefined>;
  /** Limiter bucket scope — always the *platform-or-region string* on the host. */
  scope: string;
  spec: EndpointSpec;
}

function regional(method: MethodId, region: Region, path: string, query = {}): BuiltRequest {
  return { method, host: regionHost(region), path, query, scope: region, spec: endpoint(method) };
}

function onPlatform(method: MethodId, platform: Platform, path: string, query = {}): BuiltRequest {
  return {
    method,
    host: platformHost(platform),
    path,
    query,
    scope: platform,
    spec: endpoint(method),
  };
}

/**
 * URL builders. Path segments coming from user input are encoded here — Riot
 * IDs legitimately contain spaces and non-ASCII characters.
 */
export const build = {
  accountByRiotId(region: Region, gameName: string, tagLine: string): BuiltRequest {
    return regional(
      'account.byRiotId',
      region,
      `/riot/account/v1/accounts/by-riot-id/${encodeURIComponent(gameName)}/${encodeURIComponent(tagLine)}`,
    );
  },

  accountByPuuid(region: Region, puuid: string): BuiltRequest {
    return regional(
      'account.byPuuid',
      region,
      `/riot/account/v1/accounts/by-puuid/${encodeURIComponent(puuid)}`,
    );
  },

  summonerByPuuid(platform: Platform, puuid: string): BuiltRequest {
    return onPlatform(
      'summoner.byPuuid',
      platform,
      `/lol/summoner/v4/summoners/by-puuid/${encodeURIComponent(puuid)}`,
    );
  },

  leagueEntriesByPuuid(platform: Platform, puuid: string): BuiltRequest {
    return onPlatform(
      'league.entriesByPuuid',
      platform,
      `/lol/league/v4/entries/by-puuid/${encodeURIComponent(puuid)}`,
    );
  },

  matchIdsByPuuid(
    region: Region,
    puuid: string,
    query: {
      start?: number;
      count?: number;
      queue?: number;
      type?: string;
      startTime?: number;
      endTime?: number;
    },
  ): BuiltRequest {
    return regional(
      'match.idsByPuuid',
      region,
      `/lol/match/v5/matches/by-puuid/${encodeURIComponent(puuid)}/ids`,
      query,
    );
  },

  matchById(region: Region, matchId: string): BuiltRequest {
    return regional('match.byId', region, `/lol/match/v5/matches/${encodeURIComponent(matchId)}`);
  },

  matchTimeline(region: Region, matchId: string): BuiltRequest {
    return regional(
      'match.timeline',
      region,
      `/lol/match/v5/matches/${encodeURIComponent(matchId)}/timeline`,
    );
  },

  activeGame(platform: Platform, puuid: string): BuiltRequest {
    return onPlatform(
      'spectator.activeGame',
      platform,
      `/lol/spectator/v5/active-games/by-summoner/${encodeURIComponent(puuid)}`,
    );
  },

  masteryByPuuid(platform: Platform, puuid: string): BuiltRequest {
    return onPlatform(
      'mastery.byPuuid',
      platform,
      `/lol/champion-mastery/v4/champion-masteries/by-puuid/${encodeURIComponent(puuid)}`,
    );
  },

  masteryTopByPuuid(platform: Platform, puuid: string, count: number): BuiltRequest {
    return onPlatform(
      'mastery.topByPuuid',
      platform,
      `/lol/champion-mastery/v4/champion-masteries/by-puuid/${encodeURIComponent(puuid)}/top`,
      { count },
    );
  },

  championRotations(platform: Platform): BuiltRequest {
    return onPlatform(
      'platform.championRotations',
      platform,
      '/lol/platform/v3/champion-rotations',
    );
  },

  platformStatus(platform: Platform): BuiltRequest {
    return onPlatform('status.platformData', platform, '/lol/status/v4/platform-data');
  },
};

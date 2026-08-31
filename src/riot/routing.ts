import { ProxyError } from '../errors.js';

/**
 * §5.1 — two host families. Getting this wrong is the most common integration
 * bug, so both tables live here and everything else derives from them.
 */

/** Regional hosts serve account-v1 and match-v5. */
export const REGIONS = ['americas', 'europe', 'asia', 'sea'] as const;
export type Region = (typeof REGIONS)[number];

/** Platform hosts serve summoner/league/spectator/mastery/status. */
export const PLATFORMS = [
  'na1',
  'br1',
  'la1',
  'la2',
  'euw1',
  'eun1',
  'tr1',
  'ru',
  'kr',
  'jp1',
  'oc1',
  'ph2',
  'sg2',
  'th2',
  'tw2',
  'vn2',
] as const;
export type Platform = (typeof PLATFORMS)[number];

const PLATFORM_TO_REGION: Record<Platform, Region> = {
  na1: 'americas',
  br1: 'americas',
  la1: 'americas',
  la2: 'americas',
  euw1: 'europe',
  eun1: 'europe',
  tr1: 'europe',
  ru: 'europe',
  kr: 'asia',
  jp1: 'asia',
  oc1: 'sea',
  ph2: 'sea',
  sg2: 'sea',
  th2: 'sea',
  tw2: 'sea',
  vn2: 'sea',
};

/** Human-readable labels, used by the consumer README and admin tooling. */
export const PLATFORM_LABELS: Record<Platform, string> = {
  na1: 'North America',
  br1: 'Brazil',
  la1: 'LATAM North',
  la2: 'LATAM South',
  euw1: 'EU West',
  eun1: 'EU Nordic & East',
  tr1: 'Türkiye',
  ru: 'Russia',
  kr: 'Korea',
  jp1: 'Japan',
  oc1: 'Oceania',
  ph2: 'Philippines',
  sg2: 'Singapore',
  th2: 'Thailand',
  tw2: 'Taiwan',
  vn2: 'Vietnam',
};

/** Labels for the regional hosts, same audience as PLATFORM_LABELS. */
export const REGION_LABELS: Record<Region, string> = {
  americas: 'Americas',
  europe: 'Europe',
  asia: 'Asia',
  sea: 'Southeast Asia',
};

const PLATFORM_SET = new Set<string>(PLATFORMS);
const REGION_SET = new Set<string>(REGIONS);

export function isPlatform(value: string): value is Platform {
  return PLATFORM_SET.has(value.toLowerCase());
}

export function isRegion(value: string): value is Region {
  return REGION_SET.has(value.toLowerCase());
}

export function platformToRegion(platform: Platform): Region {
  return PLATFORM_TO_REGION[platform];
}

/**
 * account-v1 is *not* served on every regional host: Riot only routes it to
 * americas/asia/europe. `sea` is a match-v5 host and 404s for account paths,
 * so SEA platforms have to borrow their nearest account host. The data is the
 * same whichever one you ask — account-v1 is a global service.
 */
export const ACCOUNT_REGIONS = ['americas', 'asia', 'europe'] as const;
export type AccountRegion = (typeof ACCOUNT_REGIONS)[number];

const REGION_TO_ACCOUNT_REGION: Record<Region, AccountRegion> = {
  americas: 'americas',
  asia: 'asia',
  europe: 'europe',
  sea: 'asia',
};

export function accountRegion(region: Region): AccountRegion {
  return REGION_TO_ACCOUNT_REGION[region];
}

/** The account host for a platform, skipping the invalid `sea` hop. */
export function platformToAccountRegion(platform: Platform): AccountRegion {
  return accountRegion(PLATFORM_TO_REGION[platform]);
}

/** All platforms routed through a given regional host. */
export function regionToPlatforms(region: Region): Platform[] {
  return PLATFORMS.filter((p) => PLATFORM_TO_REGION[p] === region);
}

export function platformHost(platform: Platform): string {
  return `${platform}.api.riotgames.com`;
}

export function regionHost(region: Region): string {
  return `${region}.api.riotgames.com`;
}

/**
 * Validate a path parameter, raising the BAD_REGION envelope (§6.1) rather
 * than a generic validation error so consumers can branch on it.
 */
export function assertPlatform(value: string): Platform {
  const lower = value.toLowerCase();
  if (!isPlatform(lower)) {
    throw ProxyError.badRegion(
      `Unknown platform '${value}'. Expected one of: ${PLATFORMS.join(', ')}`,
    );
  }
  return lower;
}

export function assertRegion(value: string): Region {
  const lower = value.toLowerCase();
  if (!isRegion(lower)) {
    throw ProxyError.badRegion(`Unknown region '${value}'. Expected one of: ${REGIONS.join(', ')}`);
  }
  return lower;
}

/**
 * Match IDs are prefixed with the platform that hosted the game
 * (e.g. `EUW1_7381937461`), which is how the archive can resolve a region
 * without the caller telling us.
 */
export function platformFromMatchId(matchId: string): Platform | undefined {
  const prefix = matchId.split('_')[0]?.toLowerCase();
  if (!prefix) return undefined;
  return isPlatform(prefix) ? prefix : undefined;
}

export function regionFromMatchId(matchId: string): Region | undefined {
  const platform = platformFromMatchId(matchId);
  return platform ? platformToRegion(platform) : undefined;
}

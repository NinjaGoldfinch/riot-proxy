import { ProxyError } from '../errors.js';

/**
 * league-v4's value space, kept beside `routing.ts` for the same reason that
 * file exists: these are closed sets Riot enforces, and the compiler is a
 * better place to learn them than a 400 from the ladder walk.
 *
 * Riot splits the ladder in two. The apex tiers are served whole by three
 * dedicated league endpoints; everything below them is walked page by page,
 * and that paged route *rejects* an apex tier outright — verified against the
 * live API:
 *
 *     GET /lol/league/v4/entries/RANKED_SOLO_5x5/MASTER/I
 *     400 invalid parameter value MASTER, must be one of
 *         [DIAMOND,EMERALD,PLATINUM,GOLD,SILVER,BRONZE,IRON]
 *
 * So the two halves are distinct types rather than one enum with a runtime
 * caveat: a builder that takes `PagedTier` cannot be handed `MASTER` at all.
 */

/** Tiers served by the paged entries route, ascending. */
export const PAGED_TIERS = [
  'IRON',
  'BRONZE',
  'SILVER',
  'GOLD',
  'PLATINUM',
  'EMERALD',
  'DIAMOND',
] as const;
export type PagedTier = (typeof PAGED_TIERS)[number];

/** Tiers served by their own league endpoint, ascending. */
export const APEX_TIERS = ['MASTER', 'GRANDMASTER', 'CHALLENGER'] as const;
export type ApexTier = (typeof APEX_TIERS)[number];

/**
 * Every tier, ascending — the order is the point. "Crawl Emerald and up" is
 * the load-bearing scope knob of the whole feature (`LADDER_TIER_FLOOR`), and
 * it needs somewhere to read the ranking from.
 */
export const TIERS = [...PAGED_TIERS, ...APEX_TIERS] as const;
export type Tier = (typeof TIERS)[number];

/** Divisions within a tier. Apex tiers report `I` for everyone. */
export const DIVISIONS = ['I', 'II', 'III', 'IV'] as const;
export type Division = (typeof DIVISIONS)[number];

/** The two ranked ladders. `RANKED_TFT_*` are a different API and out of scope. */
export const RANKED_QUEUES = ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR'] as const;
export type RankedQueue = (typeof RANKED_QUEUES)[number];

/**
 * Riot's page size for the paged route. Not a request parameter — measured
 * against the live API and quoted here because the crawl's request math (§2 of
 * docs/ladder-crawl-plan.md) is built on it.
 */
export const ENTRIES_PER_PAGE = 205;

const TIER_SET = new Set<string>(TIERS);
const APEX_SET = new Set<string>(APEX_TIERS);
const DIVISION_SET = new Set<string>(DIVISIONS);
const QUEUE_SET = new Set<string>(RANKED_QUEUES);

export function isTier(value: string): value is Tier {
  return TIER_SET.has(value.toUpperCase());
}

export function isApexTier(value: string): value is ApexTier {
  return APEX_SET.has(value.toUpperCase());
}

export function isPagedTier(value: string): value is PagedTier {
  return isTier(value) && !isApexTier(value);
}

export function isDivision(value: string): value is Division {
  return DIVISION_SET.has(value.toUpperCase());
}

export function isRankedQueue(value: string): value is RankedQueue {
  // Queue ids are the one value here that is not uppercase throughout
  // (`RANKED_SOLO_5x5`), so match Riot's own casing rather than folding.
  return QUEUE_SET.has(value);
}

/**
 * Validators, raising the VALIDATION envelope (§6.1) like the rest of the
 * proxy rather than letting a bad value reach Riot as a 400 we pay for.
 * Each normalises to Riot's casing, so `diamond` and `iv` are accepted.
 */
export function assertTier(value: string): Tier {
  const upper = value.toUpperCase();
  if (!isTier(upper)) {
    throw new ProxyError(
      'VALIDATION',
      `Unknown tier '${value}'. Expected one of: ${TIERS.join(', ')}`,
    );
  }
  return upper;
}

export function assertApexTier(value: string): ApexTier {
  const upper = value.toUpperCase();
  if (!isApexTier(upper)) {
    throw new ProxyError(
      'VALIDATION',
      `'${value}' is not an apex tier. Expected one of: ${APEX_TIERS.join(', ')}`,
    );
  }
  return upper;
}

export function assertPagedTier(value: string): PagedTier {
  const upper = value.toUpperCase();
  if (!isPagedTier(upper)) {
    // Naming the apex route in the message because asking the paged endpoint
    // for Master is the mistake this type exists to catch.
    throw new ProxyError(
      'VALIDATION',
      `Tier '${value}' is not walked page by page. Expected one of: ${PAGED_TIERS.join(', ')} ` +
        `(${APEX_TIERS.join(', ')} are served whole by the apex league endpoints)`,
    );
  }
  return upper;
}

export function assertDivision(value: string): Division {
  const upper = value.toUpperCase();
  if (!isDivision(upper)) {
    throw new ProxyError(
      'VALIDATION',
      `Unknown division '${value}'. Expected one of: ${DIVISIONS.join(', ')}`,
    );
  }
  return upper;
}

export function assertRankedQueue(value: string): RankedQueue {
  if (!isRankedQueue(value)) {
    throw new ProxyError(
      'VALIDATION',
      `Unknown ranked queue '${value}'. Expected one of: ${RANKED_QUEUES.join(', ')}`,
    );
  }
  return value;
}

/** The tier and everything above it — how a tier floor becomes a work list. */
export function tiersAtOrAbove(floor: Tier): Tier[] {
  return TIERS.slice(TIERS.indexOf(floor));
}

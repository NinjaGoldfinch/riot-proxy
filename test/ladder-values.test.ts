import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { ProxyError } from '../src/errors.js';
import {
  APEX_TIERS,
  DIVISIONS,
  PAGED_TIERS,
  RANKED_QUEUES,
  TIERS,
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
  isApexTier,
  isPagedTier,
  tiersAtOrAbove,
} from '../src/riot/ladder.js';

/**
 * league-v4's closed value sets. The split between paged and apex tiers is the
 * load-bearing part: it is verified against the live API (the paged route
 * answers `400 invalid parameter value MASTER`), and every later phase — the
 * tier floor, the crawl fan-out, the storage keys — is built on it.
 */
describe('ladder tiers, divisions and queues', () => {
  it('splits every tier into exactly one of paged or apex', () => {
    expect(TIERS).toEqual([...PAGED_TIERS, ...APEX_TIERS]);
    expect(new Set(TIERS).size).toBe(TIERS.length);
    for (const tier of TIERS) {
      expect(isApexTier(tier)).toBe(!isPagedTier(tier));
    }
  });

  it('matches the tier list Riot names in its own 400', () => {
    // Verbatim from `GET /lol/league/v4/entries/RANKED_SOLO_5x5/MASTER/I`:
    // "must be one of [DIAMOND,EMERALD,PLATINUM,GOLD,SILVER,BRONZE,IRON]".
    expect([...PAGED_TIERS].sort()).toEqual(
      ['DIAMOND', 'EMERALD', 'PLATINUM', 'GOLD', 'SILVER', 'BRONZE', 'IRON'].sort(),
    );
  });

  it('orders tiers so a floor becomes a work list', () => {
    expect(tiersAtOrAbove('MASTER')).toEqual(['MASTER', 'GRANDMASTER', 'CHALLENGER']);
    expect(tiersAtOrAbove('IRON')).toEqual([...TIERS]);
    expect(tiersAtOrAbove('CHALLENGER')).toEqual(['CHALLENGER']);
    expect(tiersAtOrAbove('EMERALD')).toContain('DIAMOND');
    expect(tiersAtOrAbove('EMERALD')).not.toContain('PLATINUM');
  });

  it("normalises casing to Riot's, so a lowercase path segment still resolves", () => {
    expect(assertTier('diamond')).toBe('DIAMOND');
    expect(assertPagedTier('iron')).toBe('IRON');
    expect(assertApexTier('challenger')).toBe('CHALLENGER');
    expect(assertDivision('iv')).toBe('IV');
  });

  it('rejects an apex tier on the paged route with a message that says where to go', () => {
    expect(() => assertPagedTier('MASTER')).toThrow(ProxyError);
    try {
      assertPagedTier('MASTER');
    } catch (error) {
      const proxy = error as ProxyError;
      expect(proxy.code).toBe('VALIDATION');
      expect(proxy.message).toContain('CHALLENGER');
    }
    // …and the reverse, so neither route can be handed the other's tiers.
    expect(() => assertApexTier('DIAMOND')).toThrow(ProxyError);
    // Still a tier, just not a walkable one.
    expect(assertTier('MASTER')).toBe('MASTER');
  });

  it('rejects values outside each set', () => {
    expect(() => assertTier('UNRANKED')).toThrow(ProxyError);
    expect(() => assertDivision('V')).toThrow(ProxyError);
    expect(() => assertRankedQueue('RANKED_TFT_DOUBLE_UP')).toThrow(ProxyError);
  });

  it("takes queue ids in Riot's own casing rather than folding them", () => {
    // `RANKED_SOLO_5x5` has a lowercase `x`; uppercasing it invents a queue.
    expect(assertRankedQueue('RANKED_SOLO_5x5')).toBe('RANKED_SOLO_5x5');
    expect(() => assertRankedQueue('RANKED_SOLO_5X5')).toThrow(ProxyError);
    expect(RANKED_QUEUES).toContain('RANKED_FLEX_SR');
  });

  it('keeps divisions to the four Riot serves', () => {
    expect([...DIVISIONS]).toEqual(['I', 'II', 'III', 'IV']);
  });
});

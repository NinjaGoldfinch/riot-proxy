import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { ProxyError } from '../src/errors.js';
import {
  ACCOUNT_REGIONS,
  PLATFORMS,
  REGIONS,
  accountRegion,
  assertPlatform,
  assertRegion,
  platformFromMatchId,
  platformHost,
  platformToAccountRegion,
  platformToRegion,
  regionFromMatchId,
  regionHost,
  regionToPlatforms,
} from '../src/riot/routing.js';

describe('routing (§5.1)', () => {
  it('maps every platform to exactly one region', () => {
    for (const platform of PLATFORMS) {
      const region = platformToRegion(platform);
      expect(REGIONS).toContain(region);
    }
  });

  it('covers every platform across the four regions with no overlap', () => {
    const seen = REGIONS.flatMap((r) => regionToPlatforms(r));
    expect(seen.sort()).toEqual([...PLATFORMS].sort());
    expect(new Set(seen).size).toBe(PLATFORMS.length);
  });

  it('matches the documented region groupings', () => {
    expect(regionToPlatforms('americas').sort()).toEqual(['br1', 'la1', 'la2', 'na1']);
    expect(regionToPlatforms('europe').sort()).toEqual(['eun1', 'euw1', 'ru', 'tr1']);
    expect(regionToPlatforms('asia').sort()).toEqual(['jp1', 'kr']);
    expect(regionToPlatforms('sea').sort()).toEqual(['oc1', 'ph2', 'sg2', 'th2', 'tw2', 'vn2']);
  });

  it('never routes account-v1 to sea, which does not serve it', () => {
    for (const platform of PLATFORMS) {
      expect(ACCOUNT_REGIONS).toContain(platformToAccountRegion(platform));
    }
    expect(accountRegion('sea')).toBe('asia');
    expect(platformToAccountRegion('oc1')).toBe('asia');
  });

  it('leaves the three real account hosts alone', () => {
    for (const region of ACCOUNT_REGIONS) expect(accountRegion(region)).toBe(region);
  });

  it('builds the two host families', () => {
    expect(platformHost('euw1')).toBe('euw1.api.riotgames.com');
    expect(regionHost('europe')).toBe('europe.api.riotgames.com');
  });

  it('accepts case-insensitive input and rejects unknown values with BAD_REGION', () => {
    expect(assertPlatform('EUW1')).toBe('euw1');
    expect(assertRegion('EUROPE')).toBe('europe');
    expect(() => assertPlatform('euw')).toThrowError(ProxyError);
    try {
      assertRegion('eu');
    } catch (err) {
      expect((err as ProxyError).code).toBe('BAD_REGION');
    }
  });

  it('derives platform and region from a match id', () => {
    expect(platformFromMatchId('EUW1_7381937461')).toBe('euw1');
    expect(regionFromMatchId('KR_1234567890')).toBe('asia');
    expect(regionFromMatchId('NOPE_1')).toBeUndefined();
  });
});

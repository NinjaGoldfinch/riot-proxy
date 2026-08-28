import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { KEY_SCOPE } from '../src/config.js';
import { cacheKey, canonicalTarget, negativeKey, scopedPurgePattern } from '../src/cache/keys.js';
import { build } from '../src/riot/endpoints.js';

describe('cache keys (§8.1)', () => {
  it('sorts query params so equivalent requests collide', () => {
    expect(canonicalTarget('/x', { start: 0, count: 20 })).toBe(
      canonicalTarget('/x', { count: 20, start: 0 }),
    );
  });

  it('drops empty and undefined params rather than hashing them', () => {
    expect(canonicalTarget('/x', { a: 1, b: undefined, c: '' })).toBe('/x?a=1');
  });

  it('distinguishes different query values', () => {
    expect(canonicalTarget('/x', { count: 20 })).not.toBe(canonicalTarget('/x', { count: 21 }));
  });

  it('namespaces by key scope, method and host (§7.4)', () => {
    const key = cacheKey(build.summonerByPuuid('euw1', 'PUUID-1'));
    expect(key.startsWith(`c:${KEY_SCOPE}:summoner.byPuuid:euw1.api.riotgames.com:`)).toBe(true);
  });

  it('keeps positive and negative namespaces distinct (§8.3)', () => {
    const req = build.activeGame('euw1', 'PUUID-1');
    expect(cacheKey(req)).not.toBe(negativeKey(req));
    expect(negativeKey(req).startsWith('neg:')).toBe(true);
  });

  it('separates the same path on different hosts', () => {
    expect(cacheKey(build.summonerByPuuid('euw1', 'P'))).not.toBe(
      cacheKey(build.summonerByPuuid('na1', 'P')),
    );
  });

  it('scopes admin purge patterns so one deployment cannot wipe another', () => {
    expect(scopedPurgePattern('summoner.*')).toBe(`c:${KEY_SCOPE}:summoner.*`);
    expect(scopedPurgePattern('c:other:*')).toBe('c:other:*');
  });
});

import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { buildPath } from '../src/riot/client.js';
import { ENDPOINTS, METHOD_IDS, build, endpoint } from '../src/riot/endpoints.js';
import { accountRegion } from '../src/riot/routing.js';

describe('endpoint specs (§5.3, §8.2)', () => {
  it('declares a spec for every method id', () => {
    expect(ENDPOINTS.map((e) => e.id).sort()).toEqual([...METHOD_IDS].sort());
  });

  it('uses the documented TTL table', () => {
    expect(endpoint('account.byRiotId').ttlSeconds).toBe(86_400);
    expect(endpoint('summoner.byPuuid').ttlSeconds).toBe(3600);
    expect(endpoint('league.entriesByPuuid').ttlSeconds).toBe(300);
    expect(endpoint('league.challenger').ttlSeconds).toBe(120);
    expect(endpoint('league.entriesByTier').ttlSeconds).toBe(120);
    expect(endpoint('match.idsByPuuid').ttlSeconds).toBe(120);
    expect(endpoint('spectator.activeGame').ttlSeconds).toBe(30);
    expect(endpoint('platform.championRotations').ttlSeconds).toBe(21_600);
    expect(endpoint('status.platformData').ttlSeconds).toBe(60);
  });

  it('treats matches and timelines as immutable', () => {
    for (const id of ['match.byId', 'match.timeline'] as const) {
      expect(endpoint(id).immutable).toBe(true);
      expect(endpoint(id).ttlSeconds).toBe(Infinity);
    }
  });

  it('routes account-v1 and match-v5 regionally, everything else per platform (§5.1)', () => {
    const regional = ENDPOINTS.filter((e) => e.host === 'regional').map((e) => e.id);
    expect(regional.sort()).toEqual(
      [
        'account.byPuuid',
        'account.byRiotId',
        'match.byId',
        'match.idsByPuuid',
        'match.timeline',
      ].sort(),
    );
  });

  it('builds regional URLs for account lookups', () => {
    const req = build.accountByRiotId('europe', 'Faker', 'KR1');
    expect(req.host).toBe('europe.api.riotgames.com');
    expect(req.path).toBe('/riot/account/v1/accounts/by-riot-id/Faker/KR1');
    expect(req.scope).toBe('europe');
  });

  it('only accepts account hosts, so a sea caller must convert first', () => {
    // @ts-expect-error — 'sea' does not serve account-v1; this is the whole point.
    build.accountByRiotId('sea', 'NinjaGoldfinch', 'OCENZ');

    const byRiotId = build.accountByRiotId(accountRegion('sea'), 'NinjaGoldfinch', 'OCENZ');
    expect(byRiotId.host).toBe('asia.api.riotgames.com');
    expect(byRiotId.scope).toBe('asia');

    const byPuuid = build.accountByPuuid(accountRegion('sea'), 'PUUID');
    expect(byPuuid.host).toBe('asia.api.riotgames.com');
    expect(byPuuid.scope).toBe('asia');
  });

  it('still routes sea match-v5 to the sea host', () => {
    expect(build.matchById('sea', 'OC1_123').host).toBe('sea.api.riotgames.com');
  });

  it('encodes Riot IDs containing spaces and non-ASCII characters', () => {
    const req = build.accountByRiotId('europe', 'Hide on bush', 'KR1');
    expect(req.path).toBe('/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1');
  });

  it('builds platform URLs for summoner lookups', () => {
    const req = build.summonerByPuuid('euw1', 'PUUID');
    expect(req.host).toBe('euw1.api.riotgames.com');
    expect(req.path).toBe('/lol/summoner/v4/summoners/by-puuid/PUUID');
  });

  it('builds one request per apex league, each with its own method id', () => {
    const challenger = build.apexLeague('euw1', 'CHALLENGER', 'RANKED_SOLO_5x5');
    expect(challenger.method).toBe('league.challenger');
    expect(challenger.host).toBe('euw1.api.riotgames.com');
    expect(challenger.path).toBe('/lol/league/v4/challengerleagues/by-queue/RANKED_SOLO_5x5');
    expect(challenger.scope).toBe('euw1');

    expect(build.apexLeague('kr', 'GRANDMASTER', 'RANKED_FLEX_SR').path).toBe(
      '/lol/league/v4/grandmasterleagues/by-queue/RANKED_FLEX_SR',
    );
    expect(build.apexLeague('kr', 'MASTER', 'RANKED_SOLO_5x5').method).toBe('league.master');
  });

  it('pages the tier walk from 1, and refuses apex tiers at compile time', () => {
    const first = build.leagueEntriesByTier('euw1', 'RANKED_SOLO_5x5', 'DIAMOND', 'I');
    expect(buildPath(first)).toBe('/lol/league/v4/entries/RANKED_SOLO_5x5/DIAMOND/I?page=1');

    const later = build.leagueEntriesByTier('euw1', 'RANKED_SOLO_5x5', 'IRON', 'IV', 42);
    expect(buildPath(later)).toBe('/lol/league/v4/entries/RANKED_SOLO_5x5/IRON/IV?page=42');

    // @ts-expect-error — the paged route 400s on apex tiers; that is the whole
    // point of splitting `PagedTier` off `Tier`.
    build.leagueEntriesByTier('euw1', 'RANKED_SOLO_5x5', 'MASTER', 'I');
  });

  it('gives every ladder read one override key, distinct from per-player league entries', () => {
    for (const id of [
      'league.challenger',
      'league.grandmaster',
      'league.master',
      'league.entriesByTier',
    ] as const) {
      expect(endpoint(id).overrideKey, id).toBe('ladder');
      expect(endpoint(id).host, id).toBe('platform');
      expect(endpoint(id).immutable, id).toBe(false);
    }
    expect(endpoint('league.entriesByPuuid').overrideKey).toBe('league');
  });

  it('serialises query params and omits empty ones', () => {
    const req = build.matchIdsByPuuid('europe', 'P', { start: 0, count: 20, queue: undefined });
    expect(buildPath(req)).toBe('/lol/match/v5/matches/by-puuid/P/ids?start=0&count=20');
  });

  it('omits the query string entirely when there are no params', () => {
    expect(buildPath(build.championRotations('euw1'))).toBe('/lol/platform/v3/champion-rotations');
  });
});

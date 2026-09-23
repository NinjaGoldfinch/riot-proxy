import { beforeAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
import { api, counter, get, metrics } from './helpers/harness.js';

/**
 * Phase 1 — resolve the operator's own account by Riot ID and pull a match by
 * id, through the proxy rather than against Riot directly.
 */
const enabled = acceptance.enabled;
const enc = encodeURIComponent;

interface Account {
  puuid: string;
  gameName: string;
  tagLine: string;
}

let puuid = '';
let matchIds: string[] = [];

describe.skipIf(!enabled)('Phase 1 — account and match lookups', () => {
  beforeAll(async () => {
    const { gameName, tagLine, region } = cfg();
    const res = await get<Account>(
      `/v1/riot/accounts/by-riot-id/${region}/${enc(gameName)}/${enc(tagLine)}`,
    );
    puuid = res.body.puuid;
  });

  it('resolves the Riot ID to a PUUID (FR-2)', () => {
    const { gameName, tagLine } = cfg();
    expect(puuid).toMatch(/^[A-Za-z0-9_-]{60,128}$/);
    // Riot echoes the canonical casing, which may differ from what we sent.
    expect(puuid.length).toBeGreaterThan(0);
    expect(gameName.length).toBeGreaterThan(0);
    expect(tagLine.length).toBeGreaterThan(0);
  });

  /**
   * The regression that motivated this suite: account-v1 is served only on
   * americas/asia/europe. A SEA platform derives `sea`, which 403s there, so
   * the proxy has to convert before dispatch — and must never open a `sea`
   * account bucket while doing it.
   */
  it('never dispatches an account lookup to a host that lacks account-v1', async () => {
    const { gameName, tagLine, region } = cfg();
    const before = await metrics();

    const res = await get<Account>(
      `/v1/riot/accounts/by-riot-id/${region}/${enc(gameName)}/${enc(tagLine)}`,
    );
    expect(res.body.puuid).toBe(puuid);

    const after = await metrics();
    for (const badScope of ['sea']) {
      const dispatched =
        counter(after, 'proxy_upstream_requests_total', {
          method: 'account.byRiotId',
          region: badScope,
        }) -
        counter(before, 'proxy_upstream_requests_total', {
          method: 'account.byRiotId',
          region: badScope,
        });
      expect(dispatched, `account.byRiotId must never dispatch to ${badScope}`).toBe(0);
    }
  });

  it('agrees across every region alias the consumer might pass', async () => {
    const { gameName, tagLine } = cfg();
    const path = `${enc(gameName)}/${enc(tagLine)}`;
    const results = await Promise.all(
      ['sea', 'asia', 'americas', 'europe'].map(async (region) => {
        const res = await api<Account>(`/v1/riot/accounts/by-riot-id/${region}/${path}`);
        return { region, status: res.status, puuid: res.body?.puuid };
      }),
    );
    for (const result of results) {
      expect(result.status, `${result.region} returned ${result.status}`).toBe(200);
      expect(result.puuid, `${result.region} resolved a different PUUID`).toBe(puuid);
    }
  });

  it('resolves the platform-host family for the same player (§5.1)', async () => {
    const { platform } = cfg();
    const summoner = await get<{ puuid: string }>(
      `/v1/lol/summoners/by-puuid/${platform}/${puuid}`,
    );
    expect(summoner.body.puuid).toBe(puuid);

    // An unranked account legitimately has no entries, so only the shape matters.
    const league = await get<unknown[]>(`/v1/lol/league/entries/by-puuid/${platform}/${puuid}`);
    expect(Array.isArray(league.body)).toBe(true);
  });

  it('lists recent match ids for the platform', async () => {
    const { region, platform } = cfg();
    const res = await get<string[]>(`/v1/lol/matches/ids/${region}/${puuid}?count=5`);
    matchIds = res.body;
    expect(Array.isArray(matchIds)).toBe(true);
    expect(matchIds.length, 'account has no match history to test against').toBeGreaterThan(0);
    for (const id of matchIds) {
      expect(id.toLowerCase()).toMatch(new RegExp(`^${platform}_\\d+$`));
    }
  });

  it('fetches a match by id and serves the repeat without touching Riot', async () => {
    const { region } = cfg();
    const matchId = matchIds[0];
    expect(matchId).toBeDefined();

    const first = await get<{ metadata: { matchId: string; participants: string[] } }>(
      `/v1/lol/matches/${region}/${matchId}`,
    );
    expect(first.body.metadata.matchId).toBe(matchId);
    expect(first.body.metadata.participants).toContain(puuid);

    // Immutable data must come from the archive on every subsequent read (§7.3).
    const before = await metrics();
    const second = await get(`/v1/lol/matches/${region}/${matchId}`);
    const after = await metrics();

    expect(second.headers.get('x-cache')).not.toBe('MISS');
    const upstream =
      counter(after, 'proxy_upstream_requests_total', { method: 'match.byId' }) -
      counter(before, 'proxy_upstream_requests_total', { method: 'match.byId' });
    expect(upstream, 'an archived match must cost zero upstream requests').toBe(0);
  });

  it('rejects a match id whose region contradicts its prefix', async () => {
    const { region } = cfg();
    const wrong = region === 'europe' ? 'americas' : 'europe';
    const res = await api<{ error: { code: string } }>(`/v1/lol/matches/${wrong}/${matchIds[0]}`);
    expect(res.status).toBe(400);
    expect(res.body.error.code).toBe('BAD_REGION');
  });
});

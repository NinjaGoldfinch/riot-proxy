import { describe, expect, it } from 'vitest';
import { summariseMatch } from '../src/routes/match-summary.js';

/**
 * The projection behind the composite match page (§6.3). What is under test is
 * what it *leaves out* as much as what it keeps: the endpoint's whole reason
 * for existing is that a page of ten full match payloads is a megabyte of
 * response for a panel that renders a champion and a scoreline.
 */

const PUUID = 'P'.repeat(78);
const OTHER = 'Q'.repeat(78);

/** A match-v5 payload, trimmed to the fields that matter here plus enough of
 *  the bulk (challenges, a second participant, teams) to prove it is dropped. */
const match = (overrides: { info?: object; participant?: object; metadata?: object } = {}) => ({
  metadata: {
    dataVersion: '2',
    matchId: 'OC1_1234567890',
    participants: [PUUID, OTHER],
    ...overrides.metadata,
  },
  info: {
    gameCreation: 1_756_000_000_000,
    gameStartTimestamp: 1_756_000_060_000,
    gameEndTimestamp: 1_756_001_894_000,
    gameDuration: 1834,
    gameMode: 'CLASSIC',
    gameName: 'teambuilder-match-1234567890',
    gameType: 'MATCHED_GAME',
    gameVersion: '15.16.673.9260',
    mapId: 11,
    queueId: 420,
    platformId: 'OC1',
    endOfGameResult: 'GameComplete',
    tournamentCode: '',
    teams: [
      { teamId: 100, win: true, bans: [{ championId: 64, pickTurn: 1 }], objectives: {} },
      { teamId: 200, win: false, bans: [], objectives: {} },
    ],
    participants: [
      {
        puuid: PUUID,
        win: true,
        gameEndedInEarlySurrender: false,
        championId: 64,
        championName: 'LeeSin',
        champLevel: 16,
        teamId: 100,
        teamPosition: 'JUNGLE',
        individualPosition: 'JUNGLE',
        lane: 'JUNGLE',
        kills: 8,
        deaths: 3,
        assists: 11,
        totalMinionsKilled: 42,
        neutralMinionsKilled: 128,
        goldEarned: 13_240,
        visionScore: 31,
        totalDamageDealtToChampions: 21_903,
        totalDamageTaken: 30_112,
        item0: 3142,
        item1: 6693,
        item2: 3814,
        item3: 3071,
        item4: 3111,
        item5: 0,
        item6: 3364,
        summoner1Id: 11,
        summoner2Id: 4,
        riotIdGameName: 'NinjaGoldfinch',
        riotIdTagline: 'OCENZ',
        perks: {
          statPerks: { defense: 5002, flex: 5008, offense: 5005 },
          styles: [
            {
              description: 'primaryStyle',
              style: 8000,
              selections: [
                { perk: 8010, var1: 1, var2: 2, var3: 3 },
                { perk: 9111, var1: 0, var2: 0, var3: 0 },
              ],
            },
            {
              description: 'subStyle',
              style: 8300,
              selections: [{ perk: 8306, var1: 0, var2: 0, var3: 0 }],
            },
          ],
        },
        // The single biggest reason a match payload is the size it is.
        challenges: Object.fromEntries(Array.from({ length: 100 }, (_, i) => [`challenge${i}`, i])),
        ...overrides.participant,
      },
      { puuid: OTHER, win: false, championId: 266, championName: 'Aatrox', kills: 2 },
    ],
    ...overrides.info,
  },
});

describe('match summary projection (§6.3)', () => {
  it('keeps the requesting player’s line and nothing else', () => {
    const summary = summariseMatch(match(), PUUID, 'OC1_1234567890');

    expect(summary).toEqual({
      matchId: 'OC1_1234567890',
      queueId: 420,
      gameMode: 'CLASSIC',
      gameVersion: '15.16.673.9260',
      gameCreation: 1_756_000_000_000,
      gameEndTimestamp: 1_756_001_894_000,
      gameDuration: 1834,
      endOfGameResult: 'GameComplete',
      player: {
        puuid: PUUID,
        win: true,
        gameEndedInEarlySurrender: false,
        championId: 64,
        championName: 'LeeSin',
        champLevel: 16,
        teamId: 100,
        teamPosition: 'JUNGLE',
        kills: 8,
        deaths: 3,
        assists: 11,
        totalMinionsKilled: 42,
        neutralMinionsKilled: 128,
        goldEarned: 13_240,
        visionScore: 31,
        totalDamageDealtToChampions: 21_903,
        item0: 3142,
        item1: 6693,
        item2: 3814,
        item3: 3071,
        item4: 3111,
        item5: 0,
        item6: 3364,
        summoner1Id: 11,
        summoner2Id: 4,
        perks: { keystone: 8010, primaryStyle: 8000, subStyle: 8300 },
      },
    });
  });

  it('drops the bulk that made the page expensive', () => {
    const serialised = JSON.stringify(summariseMatch(match(), PUUID, 'OC1_1234567890'));

    expect(serialised).not.toContain('challenge');
    expect(serialised).not.toContain('Aatrox');
    expect(serialised).not.toContain(OTHER);
    expect(serialised).not.toContain('bans');
    expect(serialised).not.toContain('statPerks');
    // The whole point, in one number: a summary is a couple of kilobytes.
    expect(serialised.length).toBeLessThan(JSON.stringify(match()).length / 4);
  });

  it('omits what Riot did not send rather than nulling it', () => {
    const sparse = {
      metadata: { matchId: 'OC1_1' },
      info: { participants: [{ puuid: PUUID, championId: 64 }] },
    };

    expect(summariseMatch(sparse, PUUID, 'OC1_1')).toEqual({
      matchId: 'OC1_1',
      player: { puuid: PUUID, championId: 64 },
    });
  });

  it('falls back to the id we asked for when the payload does not carry one', () => {
    const anonymous = { info: { participants: [{ puuid: PUUID }] } };

    expect(summariseMatch(anonymous, PUUID, 'OC1_9')?.matchId).toBe('OC1_9');
  });

  it('returns null when the match names no such player', () => {
    expect(summariseMatch(match(), OTHER.replace('Q', 'Z'), 'OC1_1234567890')).toBeNull();
    expect(summariseMatch({ metadata: { matchId: 'OC1_1' } }, PUUID, 'OC1_1')).toBeNull();
    expect(summariseMatch(null, PUUID, 'OC1_1')).toBeNull();
    expect(summariseMatch('not a match', PUUID, 'OC1_1')).toBeNull();
  });

  it('carries the Arena fields, where there is no win to render', () => {
    const arena = match({
      info: { queueId: 1700, gameMode: 'CHERRY' },
      participant: { placement: 2, playerSubteamId: 4 },
    });

    expect(summariseMatch(arena, PUUID, 'OC1_1234567890')).toMatchObject({
      queueId: 1700,
      gameMode: 'CHERRY',
      player: { placement: 2, playerSubteamId: 4 },
    });
  });

  it('leaves perks out entirely rather than guessing at a shape it cannot read', () => {
    const noRunes = match({ participant: { perks: { statPerks: {} } } });

    expect(summariseMatch(noRunes, PUUID, 'OC1_1234567890')?.player).not.toHaveProperty('perks');
  });
});

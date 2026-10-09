// The fake same-origin API both showcase suites drive the page against: the jsdom
// tests (showcase.test.mjs) and the real-browser layout tests (showcase.browser.test.mjs).
// Bodies follow the proxy's spec and Riot's documented DTOs; see the notes inline.
import { readFileSync } from 'node:fs';

const root = new URL('../../', import.meta.url);
const html = readFileSync(new URL('src/ui/showcase.html', root), 'utf8');
const LADDER = 60;

// Riot's documented shapes: league-v4 LeagueListDTO, champion-v3 ChampionInfo,
// lol-status-v4 PlatformDataDto, Data Dragon champion.json.
const entry = (i) => ({ puuid: `P${i}`, leaguePoints: 1000 + i * 10, wins: 100 + i, losses: 100, rank: 'I', hotStreak: i === LADDER - 1, veteran: false, inactive: false, freshBlood: false });
const CHAMPS = { data: { Annie: { key: '1', id: 'Annie', name: 'Annie', title: 'the Dark Child', image: { full: 'Annie.png' } }, Olaf: { key: '2', id: 'Olaf', name: 'Olaf', title: 'the Berserker', image: { full: 'Olaf.png' } } } };

// Data Dragon runesReforged.json: the five styles, a keystone of each that the match fixture uses.
const style = (id, key, file, keystones) => ({ id, key, name: key, icon: `perk-images/Styles/${file}`,
  slots: [{ runes: keystones.map(([rid, name]) => ({ id: rid, key: name.replace(/\W/g, ''), name, icon: `perk-images/Styles/${key}/${name.replace(/\W/g, '')}/${name.replace(/\W/g, '')}.png` })) }] });
const RUNES = [
  style(8000, 'Precision', '7201_Precision.png', [[8005, 'Press the Attack'], [8008, 'Lethal Tempo'], [8010, 'Conqueror']]),
  style(8100, 'Domination', '7200_Domination.png', [[8112, 'Electrocute'], [8128, 'Dark Harvest']]),
  style(8200, 'Sorcery', '7202_Sorcery.png', [[8229, 'Arcane Comet'], [8230, 'Phase Rush']]),
  style(8300, 'Inspiration', '7203_Whimsy.png', [[8369, 'First Strike']]),
  style(8400, 'Resolve', '7204_Resolve.png', [[8465, 'Guardian']]),
];

// The player view: the proxy's ProfileBody, MatchPage and PlayerChampions (openapi),
// with Riot's league-v4 LeagueEntryDTO, champion-mastery-v4 ChampionMasteryDto and
// spectator-v5 CurrentGameInfo inside.
const PUUID = 'PUUID-ME';
const line = (i) => ({ puuid: PUUID, championId: 1 + (i % 2), championName: i % 2 ? 'Olaf' : 'Annie', kills: 5, deaths: 2, assists: 9, champLevel: 16, totalMinionsKilled: 180, neutralMinionsKilled: 6, goldEarned: 12000, totalDamageDealtToChampions: 21000, summoner1Id: 4, summoner2Id: 14, perks: { keystone: 8010, primaryStyle: 8000, subStyle: 8100 }, item0: 1001, item1: 0, item4: 3047, item6: 3340, roleBoundItem: 3006, teamId: 100, win: i % 3 !== 1, gameEndedInEarlySurrender: i === 2 });
const summary = (i) => ({ matchId: `OC1_${700000 + i}`, queueId: 420, gameMode: 'CLASSIC', gameCreation: Date.now() - 3600000, gameEndTimestamp: Date.now() - 1800000, gameDuration: 1865, gameVersion: '16.19.1', player: line(i) });
function players(p, u, { live = false, refreshWait = 0 }) {
  const by = p.match(/^\/v1\/players\/by-riot-id\/([^/]+)\/([^/]+)\/profile$/);
  if (by) {
    if (decodeURIComponent(by[1]) === 'Nobody') return [404, { error: { code: 'NOT_FOUND', message: 'no such Riot ID', requestId: 'r' } }];
    return [200, { puuid: PUUID, platform: 'oc1', region: 'sea', refreshed: u.searchParams.get('refresh') === 'true', refreshAvailableIn: refreshWait, warnings: [], ageSeconds: { account: 0, summoner: 0, league: 0, mastery: 0 },
      account: { puuid: PUUID, gameName: decodeURIComponent(by[1]), tagLine: decodeURIComponent(by[2]) },
      summoner: { puuid: PUUID, profileIconId: 6, revisionDate: 1, summonerLevel: 939 },
      league: [{ queueType: 'RANKED_SOLO_5x5', tier: 'CHALLENGER', rank: 'I', puuid: PUUID, leaguePoints: 2178, wins: 410, losses: 335, veteran: false, inactive: false, freshBlood: false, hotStreak: true }],
      mastery: [{ puuid: PUUID, championId: 2, championLevel: 59, championPoints: 623792 }] }];
  }
  if (p === `/v1/players/${PUUID}/matches`) {
    const start = Number(u.searchParams.get('start'));
    const champion = Number(u.searchParams.get('champion'));
    // SITE-02: a champion's page comes from the archive, which has not reached their first game.
    if (champion) return [200, { puuid: PUUID, platform: 'oc1', region: 'sea', start, count: 10, hasMore: false, matchIdsAgeSeconds: 0, matchIdsFetchedAgeSeconds: 0, refreshed: false, refreshAvailableIn: 0, warnings: [],
      champion, archive: { complete: false }, matchIds: ['OC1_700000', 'OC1_700001'], matches: [summary(0), summary(1)] }];
    const n = start === 0 ? 10 : 3;
    return [200, { puuid: PUUID, platform: 'oc1', region: 'sea', start, count: 10, hasMore: n === 10, matchIdsAgeSeconds: 0, refreshed: false, refreshAvailableIn: 0, warnings: [],
      matchIds: Array.from({ length: n }, (_, i) => `OC1_${700000 + start + i}`), matches: Array.from({ length: n }, (_, i) => summary(start + i)),
      ...(start === 0 ? { backfill: { jobId: 'j1', status: 'queued', limit: 4294967295 } } : {}) }];
  }
  if (p === `/v1/players/${PUUID}/champions`) return [200, { puuid: PUUID, platform: 'oc1', queue: null, patch: null, archivedGames: 12, champions: [
    { championId: 2, championName: 'Olaf', games: 8, wins: 6, winRate: 0.75, avgKda: 3.5, csPerMin: 6.2, lastPlayedAt: '2026-10-01T00:00:00Z' },
    { championId: 1, championName: 'Annie', games: 4, wins: 1, winRate: 0.25, avgKda: null, csPerMin: null, lastPlayedAt: null },
  ] }];
  if (p === `/v1/lol/mastery/by-puuid/oc1/${PUUID}`) return [200, Array.from({ length: 15 }, (_, i) => ({ puuid: PUUID, championId: i + 1, championLevel: 5, championPoints: 1000 * (i + 1), lastPlayTime: 1 }))];
  if (p === `/v1/lol/spectator/active/oc1/${PUUID}`) return live
    ? [200, { gameId: 1, gameType: 'MATCHED_GAME', gameQueueConfigId: 420, gameMode: 'CLASSIC', gameStartTime: Date.now() - 600000, gameLength: 590, platformId: 'OC1', participants: [{ puuid: PUUID, championId: 1, teamId: 100, spell1Id: 4, spell2Id: 14 }, { puuid: 'X', championId: 2, teamId: 200, spell1Id: 4, spell2Id: 11 }] }]
    : [404, { error: { code: 'NOT_FOUND', message: 'not in a game', requestId: 'r' } }];
  return null;
}

// Match detail: a real match-v5 body (the replay fixture), and a timeline built on
// its participantIds the way Riot's TimelineDto carries them.
const MATCH = JSON.parse(readFileSync(new URL('tests/fixtures/replay/cold-lookup/06-match.byId.body', root), 'utf8'));
const team = (id) => MATCH.info.participants.find((x) => x.participantId === id).teamId;
const TIMELINE = { metadata: { matchId: MATCH.metadata.matchId }, info: { frameInterval: 60000, frames: Array.from({ length: 21 }, (_, m) => ({
  timestamp: m * 60000 + (m ? 37 : 0),
  // each blue player gains 20 gold a minute on red up to minute 10, then loses 60: +1,000 at 10, −4,000 at 20
  participantFrames: Object.fromEntries(MATCH.info.participants.map((x) => [String(x.participantId), { participantId: x.participantId, totalGold: 500 + m * 400 + (team(x.participantId) === 100 ? 20 * (m <= 10 ? m : 20 - 3 * m) : 0) }])),
})) } };
const DETAIL = { championId: 1, championName: 'Annie', queue: 'RANKED_SOLO_5x5', platform: 'oc1', tier: null, role: null, patch: '16.19', computedAt: '2026-10-08T00:00:00Z', totalGames: 60,
  sectionsComputedAt: { stats: null, matchups: null, items: null, runes: null, spells: null },
  stats: [
    { championId: 1, championName: 'Annie', tier: 'GOLD', patch: '16.19', games: 30, wins: 15, winRate: 0.5, share: 0.75, pickRate: 0.05, banRate: 0.01, avgKda: 2.5, csPerMin: 6.1, goldPerMin: 402.4, avgDamage: 20000, avgVision: 20 },
    // Players no ladder or lookup placed (ADR-105): listed first here, shown last.
    { championId: 1, championName: 'Annie', tier: 'UNKNOWN', patch: '16.19', games: 20, wins: 11, winRate: 0.55, share: 0.5, pickRate: 0.03, banRate: 0.01 },
    { championId: 1, championName: 'Annie', tier: 'CHALLENGER', patch: '16.19', games: 10, wins: 7, winRate: 0.7, share: 0.25, pickRate: 0.02, banRate: null },
  ],
  items: [{ itemId: 3078, games: 12, wins: 8, winRate: 0.667 }],
  spells: [{ spellA: 4, spellB: 14, games: 20, wins: 11, winRate: 0.55 }],
  runes: [{ keystoneId: 8010, subStyleId: 8100, games: 15, wins: 9, winRate: 0.6 }],
  matchups: [{ opponentId: 2, opponentName: 'Olaf', role: 'MIDDLE', games: 5, wins: 3, winRate: 0.6 }] };
const MATCHUPS = { championId: 1, championName: 'Annie', queue: 'RANKED_SOLO_5x5', platform: 'oc1', role: null, patch: '16.19', computedAt: null, matchups: [
  { opponentId: 2, opponentName: 'Olaf', role: 'MIDDLE', games: 5, wins: 3, winRate: 0.6 },
  { opponentId: 3, opponentName: 'Galio', role: 'MIDDLE', games: 9, wins: 3, winRate: 0.333 },
  { opponentId: 2, opponentName: 'Olaf', role: 'TOP', games: 2, wins: 2, winRate: 1 },
] };
function detail(p, u, { noTimeline = false, noAnalytics = false }) {
  let m;
  if ((m = p.match(/^\/v1\/lol\/matches\/([a-z]+)\/([A-Z0-9]+_\d+)(\/timeline)?$/))) {
    if (m[2] === 'OC1_1') return [404, { error: { code: 'NOT_FOUND', message: 'no match', requestId: 'r' } }];
    if (m[3]) return noTimeline ? [503, { error: { code: 'UPSTREAM_UNAVAILABLE', message: 'try later', requestId: 'r' } }] : [200, TIMELINE];
    return [200, MATCH];
  }
  // The proxy's AnalyticsPatchesResponse (DEV-21): newest first. With a championId (DEV-25), that
  // champion's games: 60 in all, as the detail's totalGames.
  if (p === '/v1/lol/analytics/patches') {
    const championId = u.searchParams.get('championId');
    const games = championId ? [3, 57] : [40, 1200];
    return [200, { platform: u.searchParams.get('platform'), queue: u.searchParams.get('queue'), championId: championId ? Number(championId) : null,
      patches: noAnalytics ? [] : [{ patch: '16.19', games: games[0], computedAt: '2026-10-08T00:00:00Z' }, { patch: '16.18', games: games[1], computedAt: '2026-10-08T00:00:00Z' }] }];
  }
  if ((m = p.match(/^\/v1\/lol\/analytics\/champions\/(\d+)(\/matchups)?$/))) {
    // 16.19 stands in for a patch too new to clear minGames: listed, but empty.
    if (noAnalytics || u.searchParams.get('patch') === '16.19') return [200, m[2] ? { ...MATCHUPS, matchups: [] } : { ...DETAIL, totalGames: 0, stats: [], items: [], spells: [], runes: [], matchups: [] }];
    return [200, m[2] ? MATCHUPS : DETAIL];
  }
  return null;
}

function api(calls, url, { status = {}, unauthorized = false, ...opts } = {}) {
  const u = new URL(url, 'http://localhost');
  calls.push(`${u.pathname}${u.search}`);
  const p = u.pathname;
  if (p === '/dev/config.json') return [200, { authDisabled: !unauthorized, version: 'test', env: 'development', platforms: [{ value: 'oc1', label: 'Oceania', region: 'sea' }, { value: 'kr', label: 'Korea', region: 'asia' }], regions: ['sea', 'asia'] }];
  if (p === '/v1/static/versions') return [200, { current: '16.19.1', versions: ['16.19.1'] }];
  if (p === '/v1/static/champion') return [200, CHAMPS];
  if (p === '/v1/static/summoner') return [200, { data: { SummonerFlash: { key: '4', image: { full: 'SummonerFlash.png' } }, SummonerDot: { key: '14', image: { full: 'SummonerDot.png' } } } }];
  if (p === '/v1/static/runes') return [200, RUNES];
  if (p === '/v1/static/item') return [200, { data: { 3078: { name: 'Trinity Force' } } }];
  if (p === '/v1/static/queues') return [200, [{ queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: null }, { queueId: 440, map: "Summoner's Rift", description: '5v5 Ranked Flex games', notes: null }]];
  if (unauthorized) return [401, { error: { code: 'UNAUTHORIZED', message: 'missing key', requestId: 'r' } }];
  const pl = players(p, u, opts) ?? detail(p, u, opts);
  if (pl) return pl;
  if (p.startsWith('/v1/lol/status/')) return [200, { id: 'OC1', name: 'Oceania', locales: ['en_US'], maintenances: [], incidents: [], ...status }];
  // `opts.apexSizes` sets a tier's list length, e.g. `{ MASTER: 10000 }` for a league at Riot's cap.
  if (p.startsWith('/v1/lol/league/apex/')) return [200, { tier: p.split('/')[6], queue: p.split('/')[7], name: 'L', leagueId: 'x', entries: Array.from({ length: opts.apexSizes?.[p.split('/')[6]] ?? LADDER }, (_, i) => entry(i)) }];
  if (p.startsWith('/v1/riot/accounts/by-puuid/')) { const id = p.split('/').pop(); return [200, { puuid: id, gameName: `Player ${id}`, tagLine: 'OCE' }]; }
  if (p.startsWith('/v1/lol/rotations/')) return [200, { freeChampionIds: [1, 2], freeChampionIdsForNewPlayers: [1], maxNewPlayerLevel: 10 }];
  if (p === '/v1/lol/analytics/champions') return [200, { platform: 'oc1', queue: u.searchParams.get('queue'), tier: null, patch: '16.19', role: null, computedAt: null, totalGames: 30, champions: [
    { championId: 1, championName: 'Annie', tier: 'GOLD', patch: '16.19', games: 10, wins: 6, winRate: 0.6, share: 0.3 },
    { championId: 1, championName: 'Annie', tier: 'MASTER', patch: '16.19', games: 10, wins: 4, winRate: 0.4, share: 0.3 },
    { championId: 2, championName: 'Olaf', tier: 'GOLD', patch: '16.19', games: 5, wins: 4, winRate: 0.8, share: 0.2 },
  ] }];
  return [404, { error: { code: 'NOT_FOUND', message: p } }];
}

export { root, html, LADDER, CHAMPS, RUNES, PUUID, MATCH, TIMELINE, DETAIL, MATCHUPS, api };

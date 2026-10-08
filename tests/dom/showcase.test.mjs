// The showcase's home (DEV-06), player (DEV-07), match and champion (DEV-08) views driven in jsdom: the real page, a fake
// same-origin API. Checks the wiring the pure-helper tests (tests/showcase.mjs)
// can't: which calls each card makes, ladder paging and names, the status banner,
// source chips and routing.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const root = new URL('../../', import.meta.url);
const html = readFileSync(new URL('src/ui/showcase.html', root), 'utf8');
const LADDER = 60;

// Riot's documented shapes: league-v4 LeagueListDTO, champion-v3 ChampionInfo,
// lol-status-v4 PlatformDataDto, Data Dragon champion.json.
const entry = (i) => ({ puuid: `P${i}`, leaguePoints: 1000 + i * 10, wins: 100 + i, losses: 100, rank: 'I', hotStreak: i === LADDER - 1, veteran: false, inactive: false, freshBlood: false });
const CHAMPS = { data: { Annie: { key: '1', id: 'Annie', name: 'Annie', title: 'the Dark Child', image: { full: 'Annie.png' } }, Olaf: { key: '2', id: 'Olaf', name: 'Olaf', title: 'the Berserker', image: { full: 'Olaf.png' } } } };

// The player view: the proxy's ProfileBody, MatchPage and PlayerChampions (openapi),
// with Riot's league-v4 LeagueEntryDTO, champion-mastery-v4 ChampionMasteryDto and
// spectator-v5 CurrentGameInfo inside.
const PUUID = 'PUUID-ME';
const line = (i) => ({ puuid: PUUID, championId: 1 + (i % 2), championName: i % 2 ? 'Olaf' : 'Annie', kills: 5, deaths: 2, assists: 9, champLevel: 16, totalMinionsKilled: 180, neutralMinionsKilled: 6, goldEarned: 12000, totalDamageDealtToChampions: 21000, summoner1Id: 4, summoner2Id: 14, item0: 1001, item1: 0, item6: 3340, teamId: 100, win: i % 3 !== 1, gameEndedInEarlySurrender: i === 2 });
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
const DETAIL = { championId: 1, championName: 'Annie', queue: 'RANKED_SOLO_5x5', platform: 'oc1', tier: null, role: null, patch: '16.19', computedAt: '2026-10-08T00:00:00Z', totalGames: 40,
  sectionsComputedAt: { stats: null, matchups: null, items: null, runes: null, spells: null },
  stats: [
    { championId: 1, championName: 'Annie', tier: 'GOLD', patch: '16.19', games: 30, wins: 15, winRate: 0.5, share: 0.75, pickRate: 0.05, banRate: 0.01, avgKda: 2.5, csPerMin: 6.1, goldPerMin: 402.4, avgDamage: 20000, avgVision: 20 },
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
function detail(p, { noTimeline = false, noAnalytics = false }) {
  let m;
  if ((m = p.match(/^\/v1\/lol\/matches\/([a-z]+)\/([A-Z0-9]+_\d+)(\/timeline)?$/))) {
    if (m[2] === 'OC1_1') return [404, { error: { code: 'NOT_FOUND', message: 'no match', requestId: 'r' } }];
    if (m[3]) return noTimeline ? [503, { error: { code: 'UPSTREAM_UNAVAILABLE', message: 'try later', requestId: 'r' } }] : [200, TIMELINE];
    return [200, MATCH];
  }
  if ((m = p.match(/^\/v1\/lol\/analytics\/champions\/(\d+)(\/matchups)?$/))) {
    if (noAnalytics) return [200, m[2] ? { ...MATCHUPS, matchups: [] } : { ...DETAIL, totalGames: 0, stats: [], items: [], spells: [], runes: [], matchups: [] }];
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
  if (p === '/v1/static/runes') return [200, [{ id: 8000, key: 'Precision', name: 'Precision', slots: [{ runes: [{ id: 8010, name: 'Conqueror' }] }] }, { id: 8100, key: 'Domination', name: 'Domination', slots: [] }]];
  if (p === '/v1/static/item') return [200, { data: { 3078: { name: 'Trinity Force' } } }];
  if (p === '/v1/static/queues') return [200, [{ queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: null }, { queueId: 440, map: "Summoner's Rift", description: '5v5 Ranked Flex games', notes: null }]];
  if (unauthorized) return [401, { error: { code: 'UNAUTHORIZED', message: 'missing key', requestId: 'r' } }];
  const pl = players(p, u, opts) ?? detail(p, opts);
  if (pl) return pl;
  if (p.startsWith('/v1/lol/status/')) return [200, { id: 'OC1', name: 'Oceania', locales: ['en_US'], maintenances: [], incidents: [], ...status }];
  if (p.startsWith('/v1/lol/league/apex/')) return [200, { tier: p.split('/')[6], queue: p.split('/')[7], name: 'L', leagueId: 'x', entries: Array.from({ length: LADDER }, (_, i) => entry(i)) }];
  if (p.startsWith('/v1/riot/accounts/by-puuid/')) { const id = p.split('/').pop(); return [200, { puuid: id, gameName: `Player ${id}`, tagLine: 'OCE' }]; }
  if (p.startsWith('/v1/lol/rotations/')) return [200, { freeChampionIds: [1, 2], freeChampionIdsForNewPlayers: [1], maxNewPlayerLevel: 10 }];
  if (p === '/v1/lol/analytics/champions') return [200, { platform: 'oc1', queue: u.searchParams.get('queue'), tier: null, patch: '16.19', role: null, computedAt: null, totalGames: 30, champions: [
    { championId: 1, championName: 'Annie', tier: 'GOLD', patch: '16.19', games: 10, wins: 6, winRate: 0.6, share: 0.3 },
    { championId: 1, championName: 'Annie', tier: 'MASTER', patch: '16.19', games: 10, wins: 4, winRate: 0.4, share: 0.3 },
    { championId: 2, championName: 'Olaf', tier: 'GOLD', patch: '16.19', games: 5, wins: 4, winRate: 0.8, share: 0.2 },
  ] }];
  return [404, { error: { code: 'NOT_FOUND', message: p } }];
}

async function page({ platform = 'oc1', hash = '', ...opts } = {}) {
  const calls = [];
  const errors = [];
  const dom = new JSDOM(html.replace('<script type="module">', '<script>(async()=>{').replace(/<\/script>\s*<\/body>/, '})()</script></body>'), {
    url: `http://localhost/dev/showcase${hash}`, runScripts: 'dangerously', pretendToBeVisual: true,
    beforeParse(w) {
      if (platform) w.localStorage.setItem('rp.showcase.platform', JSON.stringify(platform));
      w.fetch = async (url) => {
        const [status, body] = api(calls, url, opts);
        const text = JSON.stringify(body);
        return { status, headers: new Map([['x-cache', 'HIT']]), text: async () => text, json: async () => JSON.parse(text) };
      };
      w.addEventListener('error', (e) => errors.push(e.message));
      w.addEventListener('unhandledrejection', (e) => errors.push(String(e.reason)));
    },
  });
  const w = dom.window;
  const $ = (s) => w.document.querySelector(s);
  const settle = async () => { for (let i = 0; i < 20; i++) await new Promise((r) => setTimeout(r, 5)); };
  await settle();
  const click = async (sel) => { $(sel).click(); await settle(); };
  const text = (sel) => $(sel)?.textContent ?? '';
  const rows = () => [...w.document.querySelectorAll('#ladder tbody tr')];
  return { w, $, calls, errors, settle, click, text, rows };
}

test('without a platform the page asks for one and calls nothing platform-scoped', async () => {
  const p = await page({ platform: '' });
  assert.ok(p.text('#view').includes('Pick a platform'));
  assert.deepEqual(p.calls.filter((c) => c.startsWith('/v1/lol/')), []);
  assert.deepEqual(p.errors, []);
});

test('the ladder sorts by LP, pages by 25 and fills names for the visible page only', async () => {
  const p = await page();
  assert.ok(p.calls.includes('/v1/lol/league/apex/oc1/CHALLENGER/RANKED_SOLO_5x5'));
  assert.equal(p.rows().length, 25);
  const first = p.rows()[0];
  assert.equal(first.dataset.puuid, `P${LADDER - 1}`, 'highest LP first');
  assert.ok(first.textContent.includes(`Player P${LADDER - 1}`), 'name filled from account-v1');
  assert.ok(first.querySelector('.flag.hot'), 'hot streak flagged');
  assert.equal(first.dataset.href, `#/player/Player%20P${LADDER - 1}/OCE`);
  const names = () => p.calls.filter((c) => c.startsWith('/v1/riot/accounts/by-puuid/')).length;
  assert.equal(names(), 25, 'one lookup per visible row, through the any-cluster route');
  assert.ok(p.text('#ladder').includes(`page 1 of 3 · ${LADDER} players`));
  await p.click('#ladder [data-page="1"]');
  assert.ok(p.text('#ladder').includes('page 2 of 3'));
  assert.equal(names(), 50);
  await p.click('#ladder [data-page="-1"]');
  assert.equal(names(), 50, 'names are cached for the session');
  assert.equal(p.calls.filter((c) => c.startsWith('/v1/lol/league/apex/')).length, 1, 'paging does not refetch the league');
  assert.deepEqual(p.errors, []);
});

test('tier and queue toggles refetch the league; the queue also drives top champions', async () => {
  const p = await page();
  await p.click('#ladder [data-tier="MASTER"]');
  assert.ok(p.calls.includes('/v1/lol/league/apex/oc1/MASTER/RANKED_SOLO_5x5'));
  assert.ok(p.text('#ladder .tier').includes('MASTER'));
  await p.click('#ladder [data-queue="RANKED_FLEX_SR"]');
  assert.ok(p.calls.includes('/v1/lol/league/apex/oc1/MASTER/RANKED_FLEX_SR'));
  assert.ok(p.calls.includes('/v1/lol/analytics/champions?platform=oc1&queue=RANKED_FLEX_SR&limit=500'));
  assert.deepEqual(p.errors, []);
});

test('clicking a named ladder row routes to the player view', async () => {
  const p = await page();
  await p.click('#ladder tbody tr');
  assert.equal(p.w.location.hash, `#/player/Player%20P${LADDER - 1}/OCE`);
  assert.ok(p.calls.includes(`/v1/players/by-riot-id/Player%20P${LADDER - 1}/OCE/profile?platform=oc1&topMastery=3`));
  assert.ok(p.text('#pHead').includes(`Player P${LADDER - 1} #OCE`));
  assert.deepEqual(p.errors, []);
});

test('the status banner shows only when there is something to report', async () => {
  const quiet = await page();
  assert.equal(quiet.$('#status').children.length, 0);
  const busy = await page({ status: { incidents: [{ incident_severity: 'critical', titles: [{ locale: 'en_US', content: 'Login issues' }] }] } });
  assert.ok(busy.text('#status').includes('Login issues'));
  assert.ok(busy.$('#status .banner.critical'));
});

test('rotation and top champions use the local icon mirror and champion names', async () => {
  const p = await page();
  const icons = [...p.w.document.querySelectorAll('#rotation img')].map((i) => i.getAttribute('src'));
  assert.deepEqual(icons, ['/ddragon/16.19.1/img/champion/Annie.png', '/ddragon/16.19.1/img/champion/Olaf.png']);
  assert.ok(p.text('#rotation').includes('Annie'));
  const top = p.text('#top');
  assert.ok(top.includes('Highest win rate') && top.includes('Most played'));
  assert.ok(top.includes('50.0%'), 'Annie summed over two tiers: 10 / 20');
  assert.ok(top.includes('80.0%'), 'Olaf');
  assert.ok(top.includes('patch 16.19'));
});

test('every card names its calls, and the chips open the dev explorer', async () => {
  const p = await page();
  const chips = (sel) => [...p.w.document.querySelectorAll(`${sel} .src`)].map((a) => a.textContent);
  assert.deepEqual(chips('#ladder'), ['GET /v1/lol/league/apex/{platform}/{tier}/{queue}', 'GET /v1/riot/accounts/by-puuid/{puuid}']);
  assert.deepEqual(chips('#rotation'), ['GET /v1/lol/rotations/{platform}', 'GET /v1/static/{file}']);
  assert.deepEqual(chips('#top'), ['GET /v1/lol/analytics/champions']);
  assert.equal(p.$('#top .src').getAttribute('href'), '/dev#explorer?op=GET%20%2Fv1%2Flol%2Fanalytics%2Fchampions');
  assert.ok(p.text('#calls summary').match(/API calls \(\d+\)/));
  assert.ok(p.text('#calls').includes('/v1/lol/rotations/oc1'));
});

test('a missing key says so on each card instead of failing silently', async () => {
  const p = await page({ unauthorized: true });
  assert.ok(!p.$('#key').hidden, 'the key field shows when auth is on');
  assert.ok(p.text('#ladder').includes('needs a key'));
  assert.ok(p.text('#rotation').includes('needs a key'));
  assert.deepEqual(p.errors, []);
});

test('search goes to the player route; a bad Riot ID stays put', async () => {
  const p = await page();
  const form = p.$('#search');
  form.elements.id.value = 'Hide on bush#KR1';
  form.dispatchEvent(new p.w.Event('submit', { cancelable: true }));
  await p.settle();
  assert.equal(p.w.location.hash, '#/player/Hide%20on%20bush/KR1');
  form.elements.id.value = 'no tag';
  form.dispatchEvent(new p.w.Event('submit', { cancelable: true }));
  await p.settle();
  assert.equal(p.w.location.hash, '#/player/Hide%20on%20bush/KR1');
});

// ---------- player view (DEV-07)

const playerPage = (opts = {}) => page({ hash: '#/player/Faker/KR1', ...opts });

test('the player view loads the profile, then matches, pool, mastery and live game for its PUUID', async () => {
  const p = await playerPage();
  for (const c of [
    '/v1/players/by-riot-id/Faker/KR1/profile?platform=oc1&topMastery=3',
    `/v1/players/${PUUID}/matches?platform=oc1&start=0&count=10`,
    `/v1/players/${PUUID}/champions?platform=oc1&limit=10`,
    `/v1/lol/mastery/by-puuid/oc1/${PUUID}`,
    `/v1/lol/spectator/active/oc1/${PUUID}`,
    '/v1/static/queues',
    '/v1/static/summoner',
  ]) assert.ok(p.calls.includes(c), c);
  assert.ok(p.text('#pHead').includes('Faker #KR1'));
  assert.ok(p.text('#pHead').includes('Level 939'));
  assert.equal(p.$('#pHead img.avatar').getAttribute('src'), '/ddragon/16.19.1/img/profileicon/6.png');
  assert.deepEqual([...p.w.document.querySelectorAll('#pHead .mains img')].map((i) => i.alt), ['Olaf'], 'top mastery from the profile');
  assert.deepEqual(p.errors, []);
});

test('rank cards: Challenger without a division, unranked Flex', async () => {
  const p = await playerPage();
  const cards = [...p.w.document.querySelectorAll('#pRanks .rank')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.equal(cards.length, 2);
  assert.match(cards[0], /^Solo\/Duo Challenger 2178 LP\s?hot 410W 335L · 55\.0%$/);
  assert.match(cards[1], /^Flex Unranked/);
});

test('match cards: result, queue name, KDA, CS per minute, spells and items from the mirror', async () => {
  const p = await playerPage();
  const cards = [...p.w.document.querySelectorAll('#pMatches .match')];
  assert.equal(cards.length, 10);
  assert.deepEqual(cards.slice(0, 3).map((c) => c.className), ['match win', 'match loss', 'match remake']);
  const first = cards[0].textContent.replace(/\s+/g, ' ');
  for (const bit of ['Victory', '5v5 Ranked Solo games', '31:05', '5 / 2 / 9', '7.00 KDA', '186 CS', '(6.0/min)', '12,000 gold']) assert.ok(first.includes(bit), bit);
  const srcs = [...cards[0].querySelectorAll('img')].map((i) => i.getAttribute('src'));
  assert.deepEqual(srcs, [
    '/ddragon/16.19.1/img/champion/Annie.png',
    '/ddragon/16.19.1/img/spell/SummonerFlash.png', '/ddragon/16.19.1/img/spell/SummonerDot.png',
    '/ddragon/16.19.1/img/item/1001.png', '/ddragon/16.19.1/img/item/3340.png',
  ], 'empty item slots (0) have no image');
  assert.equal(cards[0].querySelectorAll('.items > *').length, 7);
  assert.ok(p.text('#pMatches').includes('queued the player\'s history for archiving (queued)'));
});

test('load more appends the next page and stops when there is no more', async () => {
  const p = await playerPage();
  await p.click('#pMatches [data-more]');
  assert.ok(p.calls.includes(`/v1/players/${PUUID}/matches?platform=oc1&start=10&count=10`));
  assert.equal(p.w.document.querySelectorAll('#pMatches .match').length, 13);
  assert.equal(p.$('#pMatches [data-more]'), null);
});

test('the queue tabs filter match history and the champion pool together', async () => {
  const p = await playerPage();
  await p.click('#pMatches [data-mqueue="420"]');
  assert.ok(p.calls.includes(`/v1/players/${PUUID}/matches?platform=oc1&start=0&count=10&queue=420`));
  assert.ok(p.calls.includes(`/v1/players/${PUUID}/champions?platform=oc1&limit=10&queue=420`));
  assert.ok(p.$('#pMatches [data-mqueue="420"]').classList.contains('on'));
  assert.deepEqual(p.errors, []);
});

test('champion pool and mastery', async () => {
  const p = await playerPage();
  const pool = [...p.w.document.querySelectorAll('#pPool tbody tr')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.deepEqual(pool, ['Olaf 8 75.0% 3.50 6.2', 'Annie 4 25.0% – –']);
  assert.ok(p.text('#pPool').includes('12 archived games'));
  assert.ok(p.text('#pMastery').includes('15 champions · 120,000 points'));
  const shown = () => p.w.document.querySelectorAll('#pMastery .champ').length;
  assert.equal(shown(), 12);
  assert.equal(p.$('#pMastery .champ').getAttribute('href'), '#/champion/15', 'highest points first');
  await p.click('#pMastery [data-mastery]');
  assert.equal(shown(), 15);
});

test('the live-game banner shows only while the player is in a game', async () => {
  const idle = await playerPage();
  assert.equal(idle.$('#pLive').children.length, 0, 'spectator 404: nothing');
  assert.deepEqual(idle.errors, []);
  const live = await playerPage({ live: true });
  const t = live.text('#pLive');
  assert.ok(t.includes('Live now') && t.includes('5v5 Ranked Solo games'));
  assert.match(t, /10:0\d/);
  assert.equal(live.w.document.querySelectorAll('#pLive .team').length, 2);
  assert.equal(live.$('#pLive .team .me img').alt, 'Annie');
});

test('refresh re-reads the profile and the match page, and waits out its window', async () => {
  const p = await playerPage();
  await p.click('#pHead [data-refresh]');
  assert.ok(p.calls.includes('/v1/players/by-riot-id/Faker/KR1/profile?platform=oc1&topMastery=3&refresh=true'));
  assert.ok(p.calls.includes(`/v1/players/${PUUID}/matches?platform=oc1&start=0&count=10&refresh=true`));
  assert.ok(p.text('#pHead').includes('Refreshed'));
  const waiting = await playerPage({ refreshWait: 42 });
  assert.ok(waiting.$('#pHead [data-refresh]').disabled);
  assert.match(waiting.$('#pHead [data-refresh]').title, /42 s/);
});

test('an unknown Riot ID says so; the search box routes to the player view', async () => {
  const p = await page({ hash: '#/player/Nobody/0000' });
  assert.ok(p.text('#pHead').includes('No player Nobody #0000'));
  assert.equal(p.calls.filter((c) => c.includes('/matches')).length, 0);
  p.$('#search').elements.id.value = 'Faker#KR1';
  p.$('#search').requestSubmit();
  await p.settle();
  assert.equal(p.w.location.hash, '#/player/Faker/KR1');
  assert.ok(p.text('#pHead').includes('Faker #KR1'));
  assert.deepEqual(p.errors, []);
});

test('every player card names its calls', async () => {
  const p = await playerPage({ live: true });
  const chips = (sel) => [...p.w.document.querySelectorAll(`${sel} .src`)].map((a) => a.textContent);
  assert.deepEqual(chips('#pHead'), ['GET /v1/players/by-riot-id/{gameName}/{tagLine}/profile', 'GET /v1/static/{file}']);
  assert.deepEqual(chips('#pRanks'), ['GET /v1/players/by-riot-id/{gameName}/{tagLine}/profile']);
  assert.deepEqual(chips('#pMatches'), ['GET /v1/players/{puuid}/matches', 'GET /v1/static/queues', 'GET /v1/static/{file}']);
  assert.deepEqual(chips('#pPool'), ['GET /v1/players/{puuid}/champions']);
  assert.deepEqual(chips('#pMastery'), ['GET /v1/lol/mastery/by-puuid/{platform}/{puuid}']);
  assert.deepEqual(chips('#pLive'), ['GET /v1/lol/spectator/active/{platform}/{puuid}']);
});

test('leaving the player view before its calls finish draws nothing over the next view', async () => {
  const p = await playerPage();
  p.w.location.hash = '#/player/Other/OCE';
  p.w.location.hash = '#/';
  await p.settle();
  assert.ok(p.$('#ladder'), 'home view');
  assert.equal(p.$('#pMatches'), null);
  assert.deepEqual(p.errors, []);
});

// ---------- match detail and champion view (DEV-08)

const matchPage = (opts = {}) => page({ hash: `#/match/asia/${MATCH.metadata.matchId}`, ...opts });

test('a match card opens its scoreboard on the region the match page named', async () => {
  const p = await page({ hash: '#/player/Faker/KR1' });
  await p.click('#pMatches article.match');
  assert.equal(p.w.location.hash, '#/match/sea/OC1_700000');
  assert.ok(p.calls.includes('/v1/lol/matches/sea/OC1_700000'));
  assert.ok(p.calls.includes('/v1/lol/matches/sea/OC1_700000/timeline'));
  assert.ok(p.text('#mHead').includes('back to Faker'));
  assert.deepEqual(p.errors, []);
});

test('a champion icon inside a match card goes to the champion, not the match', async () => {
  const p = await page({ hash: '#/player/Faker/KR1' });
  await p.click('#pMatches article.match .pick a');
  assert.equal(p.w.location.hash, '#/champion/1');
});

test('the scoreboard: two sides, totals, bans, objectives, every player line', async () => {
  const p = await matchPage();
  const cards = [...p.w.document.querySelectorAll('#mTeams .team-card')];
  assert.deepEqual(cards.map((c) => c.querySelector('h2').textContent), ['Blue side', 'Red side']);
  const blueWon = MATCH.info.teams.find((t) => t.teamId === 100).win;
  assert.equal(cards[0].className.includes(blueWon ? 'win' : 'loss'), true);
  assert.ok(cards[0].textContent.includes(blueWon ? 'Victory' : 'Defeat'));
  assert.equal(p.w.document.querySelectorAll('#mTeams tbody tr').length, 10);
  const blue = MATCH.info.participants.filter((x) => x.teamId === 100);
  const kills = blue.reduce((n, x) => n + x.kills, 0);
  assert.ok(cards[0].querySelector('header').textContent.includes(`${kills} / `));
  assert.ok(cards[0].textContent.includes(`Towers ${MATCH.info.teams[0].objectives.tower.kills}`));
  assert.equal(cards[0].querySelectorAll('.bans a').length, MATCH.info.teams[0].bans.filter((b) => b.championId > 0).length);
  const first = cards[0].querySelector('tbody tr');
  assert.ok(first.textContent.includes(blue[0].riotIdGameName));
  assert.equal(first.querySelector('td.name a').getAttribute('href'), `#/player/${encodeURIComponent(blue[0].riotIdGameName)}/${encodeURIComponent(blue[0].riotIdTagline)}`);
  assert.ok(p.text('#mHead').includes('5v5 Ranked Solo games'), 'queue name from queues.json');
  assert.ok(p.text('#mHead').includes('patch 16.19'));
  assert.deepEqual(p.errors, []);
});

test('the gold graph: one line around zero, both direct labels, a crosshair readout and a table view', async () => {
  const p = await matchPage();
  const svg = p.$('#goldChart svg');
  assert.ok(svg, 'graph drawn');
  assert.equal(svg.querySelectorAll('path.line').length, 2, 'the one series, clipped blue above zero and red below');
  const labels = [...svg.querySelectorAll('text.lbl')].map((t) => t.textContent);
  assert.deepEqual(labels, ['Blue side ahead', 'Red side ahead']);
  const rows = [...p.w.document.querySelectorAll('#mGold .table-view tbody tr')].map((r) => [...r.cells].map((c) => c.textContent));
  assert.equal(rows.length, 21);
  assert.deepEqual(rows[10], ['10', '1,000', 'Blue side']);
  assert.deepEqual(rows[20], ['20', '-4,000', 'Red side']);
  svg.dispatchEvent(new p.w.FocusEvent('focus'));
  assert.ok(!p.$('#gTip').hidden);
  assert.ok(p.text('#gTip').includes('−4.0k gold') && p.text('#gTip').includes('20 min · Red side ahead'), 'starts on the last minute');
  svg.dispatchEvent(new p.w.KeyboardEvent('keydown', { key: 'ArrowLeft' }));
  assert.ok(p.text('#gTip').includes('19 min'));
  assert.equal(p.$('#gCross').getAttribute('visibility'), 'visible');
  svg.dispatchEvent(new p.w.FocusEvent('blur'));
  assert.ok(p.$('#gTip').hidden);
});

test('a missing timeline costs only the graph; an unknown match says so', async () => {
  const p = await matchPage({ noTimeline: true });
  assert.ok(p.text('#mGold').includes('503'));
  assert.equal(p.w.document.querySelectorAll('#mTeams .team-card').length, 2);
  const gone = await page({ hash: '#/match/sea/OC1_1' });
  assert.ok(gone.text('#mHead').includes('No match OC1_1 in sea'));
  assert.deepEqual(gone.errors, []);
});

test('the champion view: rates by tier, builds with names, and the calls it makes', async () => {
  const p = await page({ hash: '#/champion/1' });
  assert.ok(p.calls.includes('/v1/lol/analytics/champions/1?queue=RANKED_SOLO_5x5&limit=10&platform=oc1'));
  assert.ok(p.calls.includes('/v1/lol/analytics/champions/1/matchups?queue=RANKED_SOLO_5x5&limit=200&platform=oc1'));
  assert.ok(p.text('#cHead').includes('Annie') && p.text('#cHead').includes('the Dark Child'));
  assert.ok(p.text('#cMeta').includes('patch 16.19 · 40 games'));
  const tiers = [...p.w.document.querySelectorAll('#cTiers tbody tr')].map((r) => r.cells[0].textContent);
  assert.deepEqual(tiers, ['CHALLENGER', 'GOLD'], 'highest tier first');
  assert.ok(p.text('#cTiers').includes('55.0% win over 40 games'), 'summed over tiers');
  const gold = p.w.document.querySelectorAll('#cTiers tbody tr')[1].textContent.replace(/\s+/g, ' ');
  for (const bit of ['30', '50.0%', '5.0%', '1.0%', '2.50', '6.1', '402']) assert.ok(gold.includes(bit), bit);
  const build = p.text('#cBuild');
  assert.ok(build.includes('Trinity Force') && build.includes('66.7%'));
  assert.ok(build.includes('Conqueror') && build.includes('+ Domination'));
  assert.deepEqual([...p.w.document.querySelectorAll('#cBuild .spells-row img')].map((i) => i.getAttribute('src')), ['/ddragon/16.19.1/img/spell/SummonerFlash.png', '/ddragon/16.19.1/img/spell/SummonerDot.png']);
  assert.deepEqual(p.errors, []);
});

test('matchups: most games first, filtered by lane', async () => {
  const p = await page({ hash: '#/champion/1' });
  const rows = () => [...p.w.document.querySelectorAll('#cMatchups tbody tr')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.deepEqual([...p.w.document.querySelectorAll('#cMatchups tbody tr')].map((r) => [r.cells[2].textContent, r.cells[1].textContent]), [['9', 'Middle'], ['5', 'Middle'], ['2', 'Top']]);
  assert.ok(rows()[0].includes('33.3%'), 'Galio, 9 games, first');
  assert.deepEqual([...p.w.document.querySelectorAll('#cMatchups [data-role]')].map((b) => b.textContent), ['All', 'Top', 'Middle']);
  await p.click('#cMatchups [data-role="TOP"]');
  assert.equal(rows().length, 1);
  assert.ok(rows()[0].includes('Top') && rows()[0].includes('100.0%'));
});

test('the champion queue tabs refetch both calls for the other queue', async () => {
  const p = await page({ hash: '#/champion/1' });
  await p.click('#cHead [data-cqueue="RANKED_FLEX_SR"]');
  assert.ok(p.calls.includes('/v1/lol/analytics/champions/1?queue=RANKED_FLEX_SR&limit=10&platform=oc1'));
  assert.ok(p.calls.includes('/v1/lol/analytics/champions/1/matchups?queue=RANKED_FLEX_SR&limit=200&platform=oc1'));
  assert.ok(p.$('#cHead [data-cqueue="RANKED_FLEX_SR"]').classList.contains('on'));
});

test('without a platform the champion view sums every platform; without data it says so', async () => {
  const p = await page({ hash: '#/champion/1', platform: '' });
  assert.ok(p.calls.includes('/v1/lol/analytics/champions/1?queue=RANKED_SOLO_5x5&limit=10'));
  assert.ok(p.text('#cMeta').startsWith('every platform'));
  const empty = await page({ hash: '#/champion/1', noAnalytics: true });
  assert.ok(empty.text('#cTiers').includes('No analytics for Annie yet'));
  assert.ok(empty.text('#cMatchups').includes('No lane matchups'));
  assert.deepEqual(empty.errors, []);
});

test('every match and champion card names its calls', async () => {
  const m = await matchPage();
  const chips = (p, sel) => [...p.w.document.querySelectorAll(`${sel} .src`)].map((a) => a.textContent);
  assert.deepEqual(chips(m, '#mHead'), ['GET /v1/lol/matches/{region}/{matchId}', 'GET /v1/lol/matches/{region}/{matchId}/timeline', 'GET /v1/static/queues', 'GET /v1/static/{file}']);
  assert.deepEqual(chips(m, '#mGold'), ['GET /v1/lol/matches/{region}/{matchId}/timeline']);
  const c = await page({ hash: '#/champion/1' });
  assert.deepEqual(chips(c, '#cTiers'), ['GET /v1/lol/analytics/champions/{championId}']);
  assert.deepEqual(chips(c, '#cMatchups'), ['GET /v1/lol/analytics/champions/{championId}/matchups']);
});

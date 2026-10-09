// Unit tests for the showcase's pure helpers (DEV-06, DEV-07, DEV-08, BLD-04). They live in one marked
// block of src/ui/showcase.html, which has no build step, so this test cuts that
// block out and evaluates it. Run by `cargo test` (tests/ui.rs) when node is on PATH:
//   node --test tests/showcase.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../src/ui/showcase.html', import.meta.url), 'utf8');
const block = html.match(/\/\/ -{10} pure helpers[^\n]*\n([\s\S]*?)\/\/ -{10} end pure helpers/);
assert.ok(block, 'the pure helpers block is marked in showcase.html');
const h = new Function(`${block[1]}; return { APEX, QUEUES, LADDER_PAGE, APEX_LIST_CAP, apexCapNote, tierColour, winRate, pct, sortLadder, ladderPage, championIndex, imageUrl, statusNotices, topChampions, parseRiotId, parseRoute, explorerLink, rankLabel, rankCards, queueNames, spellIndex, outcome, kda, durationSecs, clock, ago, masterySummary, liveGame, TIERS, SIDES, matchHref, matchTeams, goldDiff, goldScale, signedGold, tierRows, runeIndex, runePair, runeUrl, itemIndex, bootsIndex, itemSlots, roleList, thousands, patchChoices, pickPatch, patchLabel, BUILD_STARTER_MS, SHOP_VISIT_GAP_MS, itemCatalog, playerBuild, shopVisits };`)();

test('apex tiers and queues are the ones the apex route accepts', () => {
  assert.deepEqual(h.APEX, ['CHALLENGER', 'GRANDMASTER', 'MASTER']);
  assert.deepEqual(h.QUEUES.map(([q]) => q), ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR']);
  assert.deepEqual(h.QUEUES.map(([, , id]) => id), [420, 440], "their queueIds in Riot's queues.json");
});

test('every tier has its own colour, case-insensitively, and unknown tiers are grey', () => {
  const tiers = ['IRON', 'BRONZE', 'SILVER', 'GOLD', 'PLATINUM', 'EMERALD', 'DIAMOND', 'MASTER', 'GRANDMASTER', 'CHALLENGER'];
  assert.equal(new Set(tiers.map(h.tierColour)).size, tiers.length);
  assert.equal(h.tierColour('gold'), h.tierColour('GOLD'));
  assert.equal(h.tierColour('UNRANKED'), '#7d8794');
});

test('win rate and percentages', () => {
  assert.equal(h.winRate(3, 1), 0.75);
  assert.equal(h.winRate(0, 0), null);
  assert.equal(h.pct(0.5234), '52.3%');
  assert.equal(h.pct(null), '–');
});

test("a league at Riot's apex cap gets a note, one under it does not", () => {
  assert.equal(h.APEX_LIST_CAP, 10000, 'RIOT_APEX_LIST_CAP in src/riot/ladder.rs');
  assert.equal(h.apexCapNote('MASTER', 10000), 'Master: top 10,000 only (Riot API limit)');
  assert.equal(h.apexCapNote('MASTER', 9999), '');
  assert.equal(h.apexCapNote('CHALLENGER', 300), '');
  assert.equal(h.apexCapNote('GRANDMASTER', 700), '');
});

test('the ladder sorts by LP, then wins, and numbers positions from 1', () => {
  const sorted = h.sortLadder([
    { puuid: 'a', leaguePoints: 900, wins: 10 },
    { puuid: 'b', leaguePoints: 1200, wins: 5 },
    { puuid: 'c', leaguePoints: 900, wins: 30 },
  ]);
  assert.deepEqual(sorted.map((e) => [e.puuid, e.position]), [['b', 1], ['c', 2], ['a', 3]]);
  assert.deepEqual(h.sortLadder(undefined), []);
});

test('ladder pages are 25 rows and clamp to the ends', () => {
  const sorted = Array.from({ length: 60 }, (_, i) => ({ position: i + 1 }));
  assert.equal(h.LADDER_PAGE, 25);
  const first = h.ladderPage(sorted, 0);
  assert.deepEqual([first.page, first.pages, first.rows.length, first.rows[0].position], [0, 3, 25, 1]);
  const last = h.ladderPage(sorted, 9);
  assert.deepEqual([last.page, last.rows.length, last.rows[0].position], [2, 10, 51]);
  assert.deepEqual(h.ladderPage([], -1), { page: 0, pages: 1, rows: [] });
});

test('champion.json is indexed by numeric key', () => {
  const idx = h.championIndex({ data: { Aatrox: { key: '266', id: 'Aatrox', name: 'Aatrox', title: 'the Darkin Blade', image: { full: 'Aatrox.png' } } } });
  assert.deepEqual(idx[266], { id: 'Aatrox', name: 'Aatrox', title: 'the Darkin Blade', image: 'Aatrox.png' });
  assert.deepEqual(h.championIndex(null), {});
});

test('image URLs point at the local mirror, and need a patch and a file', () => {
  assert.equal(h.imageUrl('16.19.1', 'champion', 'Aatrox.png'), '/ddragon/16.19.1/img/champion/Aatrox.png');
  assert.equal(h.imageUrl('16.19.1', 'champion', 'Kai Sa.png'), '/ddragon/16.19.1/img/champion/Kai%20Sa.png');
  assert.equal(h.imageUrl(null, 'champion', 'Aatrox.png'), null);
  assert.equal(h.imageUrl('16.19.1', 'champion', undefined), null);
});

test('status notices list maintenances and incidents in the wanted locale', () => {
  const doc = {
    maintenances: [{ maintenance_status: 'scheduled', titles: [{ locale: 'de_DE', content: 'Wartung' }, { locale: 'en_US', content: 'Maintenance' }] }],
    incidents: [{ incident_severity: 'critical', titles: [{ locale: 'ko_KR', content: '장애' }] }, { incident_severity: 'info', titles: [] }],
  };
  assert.deepEqual(h.statusNotices(doc, 'de_DE'), [
    { kind: 'Maintenance', severity: 'scheduled', title: 'Wartung' },
    { kind: 'Incident', severity: 'critical', title: '장애' },
    { kind: 'Incident', severity: 'info', title: '(untitled)' },
  ]);
  assert.equal(h.statusNotices(doc, 'fr_FR')[0].title, 'Maintenance', 'falls back to en_US');
  assert.deepEqual(h.statusNotices({ maintenances: [], incidents: [] }), []);
});

test('top champions rank the summed rows by win rate and by games', () => {
  // The route's rows without a tier: one per champion, every tier summed (ADR-123).
  const rows = [
    { championId: 1, championName: 'Annie', tier: null, games: 20, wins: 10, winRate: 0.5 },
    { championId: 2, championName: 'Olaf', tier: null, games: 4, wins: 3, winRate: 0.75 },
  ];
  const top = h.topChampions(rows, 1);
  assert.deepEqual(top.winRate.map((c) => c.championId), [2]);
  assert.deepEqual(top.played.map((c) => c.championId), [1]);
  assert.deepEqual(h.topChampions(undefined), { winRate: [], played: [] });
});

test('Riot IDs parse as Name#TAG', () => {
  assert.deepEqual(h.parseRiotId(' Hide on bush #KR1 '), { gameName: 'Hide on bush', tagLine: 'KR1' });
  assert.deepEqual(h.parseRiotId('a#b#c'), null);
  for (const bad of ['', 'nohash', '#TAG', 'Name#', null]) assert.equal(h.parseRiotId(bad), null, String(bad));
});

test('hash routes', () => {
  assert.deepEqual(h.parseRoute(''), { view: 'home', args: {} });
  assert.deepEqual(h.parseRoute('#/'), { view: 'home', args: {} });
  assert.deepEqual(h.parseRoute('#/player/Hide%20on%20bush/KR1'), { view: 'player', args: { gameName: 'Hide on bush', tagLine: 'KR1' } });
  assert.deepEqual(h.parseRoute('#/champion/266'), { view: 'champion', args: { id: 266 } });
  assert.deepEqual(h.parseRoute('#/champion/abc'), { view: 'home', args: {} });
  assert.deepEqual(h.parseRoute('#/nowhere'), { view: 'home', args: {} });
  assert.deepEqual(h.parseRoute('#/match/sea/OC1_700123'), { view: 'match', args: { region: 'sea', matchId: 'OC1_700123' } });
  for (const bad of ['#/match/SEA/OC1_1', '#/match/sea/oc1-1', '#/match/sea']) assert.equal(h.parseRoute(bad).view, 'home', bad);
});

test('source chips open the operation in the dev explorer', () => {
  assert.equal(h.explorerLink('GET /v1/lol/status/{platform}'), '/dev#explorer?op=GET%20%2Fv1%2Flol%2Fstatus%2F%7Bplatform%7D');
});

// ---------- player view (DEV-07)

test('rank labels: apex tiers have no division, and no entry is unranked', () => {
  assert.equal(h.rankLabel({ tier: 'GOLD', rank: 'II' }), 'Gold II');
  assert.equal(h.rankLabel({ tier: 'CHALLENGER', rank: 'I' }), 'Challenger');
  assert.equal(h.rankLabel(null), 'Unranked');
});

test('rank cards: Solo/Duo and Flex always, in that order, then any other queue', () => {
  const solo = { queueType: 'RANKED_SOLO_5x5', tier: 'GOLD', rank: 'I' };
  const other = { queueType: 'RANKED_TFT_DOUBLE_UP', tier: 'IRON', rank: 'IV' };
  const cards = h.rankCards([other, solo]);
  assert.deepEqual(cards.map((c) => [c.queueType, c.label, c.entry]), [
    ['RANKED_SOLO_5x5', 'Solo/Duo', solo],
    ['RANKED_FLEX_SR', 'Flex', null],
    ['RANKED_TFT_DOUBLE_UP', 'RANKED_TFT_DOUBLE_UP', other],
  ]);
  assert.deepEqual(h.rankCards(null).map((c) => c.entry), [null, null]);
});

test('queues.json and summoner.json are indexed by id', () => {
  assert.deepEqual(h.queueNames([{ queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games' }, { queueId: 0, map: 'Custom games', description: null }]), { 420: '5v5 Ranked Solo games' });
  assert.deepEqual(h.queueNames(undefined), {});
  assert.deepEqual(h.spellIndex({ data: { SummonerFlash: { key: '4', image: { full: 'SummonerFlash.png' } } } }), { 4: 'SummonerFlash.png' });
  assert.deepEqual(h.spellIndex(null), {});
});

test('match outcome: Arena placement, then remake, then win or loss', () => {
  assert.deepEqual(h.outcome({ placement: 3, win: false }), { kind: 'place', label: '#3' });
  assert.deepEqual(h.outcome({ gameEndedInEarlySurrender: true, win: false }), { kind: 'remake', label: 'Remake' });
  assert.equal(h.outcome({ win: true }).kind, 'win');
  assert.equal(h.outcome({ win: false }).kind, 'loss');
  assert.equal(h.outcome(undefined).kind, 'unknown');
});

test('KDA floors deaths at 1, as the champion pool does', () => {
  assert.equal(h.kda(5, 2, 9), 7);
  assert.equal(h.kda(3, 0, 4), 7);
  assert.equal(h.kda(undefined, undefined, undefined), 0);
});

test('game duration is seconds, or milliseconds on matches without gameEndTimestamp', () => {
  assert.equal(h.durationSecs({ gameDuration: 1865, gameEndTimestamp: 1 }), 1865);
  assert.equal(h.durationSecs({ gameDuration: 1865000 }), 1865);
  assert.equal(h.durationSecs({}), null);
  assert.equal(h.clock(1865), '31:05');
  assert.equal(h.clock(3729), '1:02:09');
  assert.equal(h.clock(null), '–');
});

test('time ago', () => {
  const now = 10 * 86400000;
  assert.equal(h.ago(now - 5 * 60000, now), '5 min ago');
  assert.equal(h.ago(now - 3 * 3600000, now), '3 h ago');
  assert.equal(h.ago(now - 4 * 86400000, now), '4 d ago');
  assert.equal(h.ago(null, now), '');
});

test('mastery is sorted by points and totalled', () => {
  const m = h.masterySummary([{ championId: 1, championPoints: 10 }, { championId: 2, championPoints: 300 }]);
  assert.deepEqual([m.champions, m.points, m.list.map((x) => x.championId)], [2, 310, [2, 1]]);
  assert.deepEqual(h.masterySummary(undefined), { champions: 0, points: 0, list: [] });
});

test('a live game splits into teams, marks the player and counts time from gameStartTime', () => {
  const doc = { gameQueueConfigId: 420, gameMode: 'CLASSIC', gameStartTime: 1000, gameLength: 5, participants: [
    { puuid: 'b', teamId: 200, championId: 2 }, { puuid: 'a', teamId: 100, championId: 1 }, { puuid: 'c', teamId: 100, championId: 3 },
  ] };
  const g = h.liveGame(doc, 'a', 1000 + 125000);
  assert.equal(g.queueId, 420);
  assert.equal(g.secs, 125);
  assert.equal(g.me.championId, 1);
  assert.deepEqual(g.teams.map((t) => [t.teamId, t.players.map((p) => [p.championId, p.me])]), [[100, [[1, true], [3, false]]], [200, [[2, false]]]]);
  assert.equal(h.liveGame({ ...doc, gameStartTime: 0 }, 'a').secs, 5, 'still loading: Riot\'s gameLength');
  assert.equal(h.liveGame(null, 'a').me, null);
});

// ---------- match detail and champion view (DEV-08)

const fixture = JSON.parse(readFileSync(new URL('fixtures/replay/cold-lookup/06-match.byId.body', import.meta.url), 'utf8'));

test('match links', () => {
  assert.equal(h.matchHref('sea', 'OC1_700123'), '#/match/sea/OC1_700123');
});

test('a Summoner\'s Rift match splits into blue and red side with totals, bans and objectives', () => {
  const me = fixture.info.participants[7].puuid;
  const teams = h.matchTeams(fixture, me);
  assert.deepEqual(teams.map((t) => [t.key, t.label, t.players.length]), [[100, 'Blue side', 5], [200, 'Red side', 5]]);
  assert.equal(teams.filter((t) => t.win).length, 1, 'exactly one winner');
  const blue = fixture.info.participants.filter((p) => p.teamId === 100);
  assert.equal(teams[0].totals.kills, blue.reduce((n, p) => n + p.kills, 0));
  assert.equal(teams[0].totals.gold, blue.reduce((n, p) => n + p.goldEarned, 0));
  assert.deepEqual(teams[0].bans, fixture.info.teams[0].bans.map((b) => b.championId).filter((id) => id > 0));
  assert.equal(teams[0].objectives.tower.kills, fixture.info.teams[0].objectives.tower.kills);
  assert.equal(teams.flatMap((t) => t.players).filter((p) => p.me).length, 1);
  assert.equal(teams[1].players.find((p) => p.me).puuid, me);
  const p = teams[0].players[0];
  assert.equal(p.cs, blue[0].totalMinionsKilled + blue[0].neutralMinionsKilled);
});

test('an Arena match groups by subteam, ordered by placement', () => {
  const arena = { info: { teams: [], participants: [
    { puuid: 'a', teamId: 100, playerSubteamId: 3, placement: 2, kills: 1 }, { puuid: 'b', teamId: 100, playerSubteamId: 3, placement: 2, kills: 2 },
    { puuid: 'c', teamId: 200, playerSubteamId: 1, placement: 1, kills: 4 }, { puuid: 'd', teamId: 200, playerSubteamId: 1, placement: 1, kills: 0 },
  ] } };
  const teams = h.matchTeams(arena, 'b');
  assert.deepEqual(teams.map((t) => [t.key, t.label, t.win, t.totals.kills]), [[1, '#1', null, 4], [3, '#2', null, 3]]);
  assert.deepEqual(h.matchTeams(null, 'x'), []);
});

test('gold difference is blue minus red per frame, sided by the match\'s participantId', () => {
  const match = { info: { participants: [{ participantId: 1, teamId: 100 }, { participantId: 2, teamId: 200 }, { participantId: 3, teamId: 200 }] } };
  const timeline = { info: { frames: [
    { timestamp: 0, participantFrames: { 1: { participantId: 1, totalGold: 500 }, 2: { participantId: 2, totalGold: 500 }, 3: { participantId: 3, totalGold: 500 } } },
    { timestamp: 60012, participantFrames: { 1: { participantId: 1, totalGold: 2600 }, 2: { participantId: 2, totalGold: 700 }, 3: { participantId: 3, totalGold: 900 } } },
  ] } };
  assert.deepEqual(h.goldDiff(timeline, match), [{ minute: 0, diff: -500 }, { minute: 1, diff: 1000 }]);
  const arena = { info: { participants: [{ participantId: 1, teamId: 100 }, { participantId: 2, teamId: 300 }] } };
  assert.deepEqual(h.goldDiff(timeline, arena), [], 'not two sides: no graph');
  assert.deepEqual(h.goldDiff(null, match), []);
});

test('the gold axis is symmetric with a round step, and leads read signed', () => {
  assert.deepEqual(h.goldScale([{ diff: 300 }]), { max: 2000, step: 1000 });
  assert.deepEqual(h.goldScale([{ diff: -3500 }, { diff: 1200 }]), { max: 4000, step: 2000 });
  assert.deepEqual(h.goldScale([{ diff: 9000 }]), { max: 10000, step: 5000 });
  assert.equal(h.signedGold(2300), '+2.3k');
  assert.equal(h.signedGold(-800), '−800');
  assert.equal(h.signedGold(0), '0');
});

test('stat rows sort highest tier first, UNKNOWN last', () => {
  assert.equal(h.TIERS.length, 10);
  const rows = h.tierRows([{ tier: 'UNKNOWN' }, { tier: 'GOLD' }, { tier: 'CHALLENGER' }]);
  assert.deepEqual(rows.map((r) => r.tier), ['CHALLENGER', 'GOLD', 'UNKNOWN']);
  assert.deepEqual(h.tierRows(undefined), []);
});

test('runesReforged.json and item.json are indexed by id', () => {
  const doc = [{ id: 8000, key: 'Precision', name: 'Precision', icon: 'perk-images/Styles/7201_Precision.png', slots: [{ runes: [{ id: 8010, name: 'Conqueror', icon: 'perk-images/Styles/Precision/Conqueror/Conqueror.png' }] }] }];
  assert.deepEqual(h.runeIndex(doc), {
    8000: { name: 'Precision', icon: 'perk-images/Styles/7201_Precision.png' },
    8010: { name: 'Conqueror', icon: 'perk-images/Styles/Precision/Conqueror/Conqueror.png' },
  });
  assert.deepEqual(h.runeIndex(null), {});
  assert.deepEqual(h.itemIndex({ data: { 3078: { name: 'Trinity Force' } } }), { 3078: 'Trinity Force' });
});

test('the item grid: inventory closed up, the trinket in cell 4, boots in cell 8 (DEV-31)', () => {
  // Data Dragon item.json tags boots "Boots"; Stealth Ward is a trinket, Trinity Force neither.
  const boots = h.bootsIndex({ data: { 3006: { name: "Berserker's Greaves", tags: ['Boots', 'AttackSpeed'] }, 3047: { name: 'Plated Steelcaps', tags: ['Boots'] }, 3078: { name: 'Trinity Force', tags: ['Damage'] }, 3340: { name: 'Stealth Ward', tags: ['Trinket'] }, 1: { name: 'untagged' } } });
  assert.deepEqual([...boots].sort(), [3006, 3047]);
  assert.deepEqual([...h.bootsIndex(null)], []);
  const p = { item0: 3078, item1: 0, item2: 3047, item3: 0, item4: 6672, item5: 0, item6: 3340, roleBoundItem: 3006 };
  assert.deepEqual(h.itemSlots({ ...p, teamPosition: 'BOTTOM' }, boots), [3078, 3047, 6672, 3340, 0, 0, 0, 3006], 'a bot laner\'s role quest boots go to cell 8');
  assert.deepEqual(h.itemSlots({ ...p, teamPosition: 'BOTTOM', roleBoundItem: 0 }, boots), [3078, 6672, 0, 3340, 0, 0, 0, 3047], 'quest not done: their inventory boots go there');
  assert.deepEqual(h.itemSlots({ ...p, teamPosition: 'JUNGLE', roleBoundItem: 1209 }, boots), [3078, 6672, 0, 3340, 0, 0, 0, 3047], 'other roles\' quest rewards are not shown');
  assert.deepEqual(h.itemSlots({ ...p, teamPosition: 'JUNGLE', item2: 0 }, boots), [3078, 6672, 0, 3340, 0, 0, 0, 0], 'no boots: cell 8 is empty');
  const full = { item0: 1, item1: 2, item2: 3, item3: 4, item4: 5, item5: 6, item6: 3340 };
  assert.deepEqual(h.itemSlots(full, boots), [1, 2, 3, 3340, 4, 5, 6, 0], 'six items fill cells 1–3 and 5–7');
  assert.deepEqual(h.itemSlots({ ...full, item2: 3047 }, null), [1, 2, 3047, 3340, 4, 5, 6, 0], 'without item.json the boots stay in the inventory');
  assert.deepEqual(h.itemSlots({ ...p, teamPosition: '', item6: 0, roleBoundItem: undefined }, boots), [3078, 6672, 0, 0, 0, 0, 0, 3047], 'no trinket (Arena): cell 4 empty');
});

test('keystone and secondary style from match-v5 perks or the proxy\'s summary', () => {
  // match-v5 PerksDto: styles tagged primaryStyle / subStyle, the keystone first in the primary's selections.
  const perks = { statPerks: { defense: 5001, flex: 5008, offense: 5005 }, styles: [
    { description: 'subStyle', style: 8300, selections: [{ perk: 8304 }, { perk: 8345 }] },
    { description: 'primaryStyle', style: 8200, selections: [{ perk: 8230 }, { perk: 8226 }, { perk: 8210 }, { perk: 8237 }] },
  ] };
  assert.deepEqual(h.runePair({ perks }), { keystone: 8230, subStyle: 8300 });
  assert.deepEqual(h.runePair({ perks: { keystone: 8010, primaryStyle: 8000, subStyle: 8100 } }), { keystone: 8010, subStyle: 8100 });
  assert.deepEqual(h.runePair({ perks: { primaryStyle: 8000 } }), { keystone: null, subStyle: null });
  assert.deepEqual(h.runePair({}), { keystone: null, subStyle: null });
});

test('rune icons come from the mirror, path segments encoded, perk-images only', () => {
  assert.equal(h.runeUrl('16.19.1', 'perk-images/Styles/Precision/Conqueror/Conqueror.png'), '/ddragon/16.19.1/img/perk-images/Styles/Precision/Conqueror/Conqueror.png');
  assert.equal(h.runeUrl('16.19.1', 'perk-images/Styles/A B.png'), '/ddragon/16.19.1/img/perk-images/Styles/A%20B.png');
  assert.equal(h.runeUrl('16.19.1', 'img/champion/Ahri.png'), null);
  assert.equal(h.runeUrl(null, 'perk-images/Styles/7201_Precision.png'), null);
  assert.equal(h.runeUrl('16.19.1', undefined), null);
});

test('matchup lanes come back in teamPosition order', () => {
  assert.deepEqual(h.roleList([{ role: 'MIDDLE' }, { role: 'TOP' }, { role: 'MIDDLE' }]), ['TOP', 'MIDDLE']);
  assert.deepEqual(h.roleList(undefined), []);
  assert.deepEqual(h.SIDES, { 100: 'Blue side', 200: 'Red side' });
});

test('thousands shorten to one decimal from 1,000', () => {
  assert.equal(h.thousands(12000), '12.0k');
  assert.equal(h.thousands(21345), '21.3k');
  assert.equal(h.thousands(999), '999');
  assert.equal(h.thousands(undefined), '0');
});

test('the patch picker offers every patch first, then each patch, and falls back to every patch', () => {
  const list = [{ patch: '16.19', games: 10 }, { patch: '16.18', games: 1406 }];
  assert.deepEqual(h.patchChoices(list), [['all', 'All patches · 1,416 games'], ['16.19', '16.19 · 10 games'], ['16.18', '16.18 · 1,406 games']]);
  assert.deepEqual(h.patchChoices(undefined), [['all', 'All patches · 0 games']]);
  assert.equal(h.pickPatch('16.18', list), '16.18');
  assert.equal(h.pickPatch('16.17', list), 'all', 'a patch this ladder lacks');
  assert.equal(h.pickPatch('all', []), 'all');
  assert.equal(h.patchLabel('all'), 'all patches');
  assert.equal(h.patchLabel('16.19'), 'patch 16.19');
});

// ---------- one player's build (BLD-04): the same rules as src/archive/builds.rs, held to one golden file
const BLD = (f) => JSON.parse(readFileSync(new URL(`fixtures/builds/${f}`, import.meta.url), 'utf8'));
const CATALOG = h.itemCatalog(BLD('item-16.19.1.json'));

test("item.json's rules: finished items, boots, trinkets and what each is built from", () => {
  const is = (id) => CATALOG.get(id);
  assert.equal(is(3078).finished, true, 'Trinity Force');
  assert.equal(is(1036).finished, false, 'Long Sword builds into things');
  assert.equal(is(2003).finished, false, 'a potion is a consumable');
  assert.equal(is(1055).finished, false, "Doran's Blade is under 1500 gold");
  assert.deepEqual([is(3006).finished, is(3006).bootsTag], [false, true], "Berserker's Greaves are boots, not finished");
  assert.deepEqual([is(3172).bootsTag, is(3172).from], [false, [3006]], 'Gunmetal Greaves have no Boots tag');
  assert.equal(is(3340).trinket, true, 'Stealth Ward');
  const doc = { data: { 1: { gold: { total: 2000, purchasable: true }, tags: [] }, 2: { gold: { total: 2000, purchasable: false } }, x: { gold: { total: 9999, purchasable: true } } } };
  const c = h.itemCatalog(doc);
  assert.deepEqual([...c.keys()], [1, 2], 'an id that is not a number is left out');
  assert.deepEqual([c.get(1).finished, c.get(2).finished], [true, false], 'an item no one can buy is not finished');
  assert.equal(h.itemCatalog(null).size, 0);
});

/** A timeline of one player (participantId 1, puuid P) with these events, Riot's TimelineDto shape. */
const timeline = (events, participants = [{ participantId: 1, puuid: 'P' }]) => ({ metadata: {}, info: { participants, frames: [{ timestamp: 0, events }] } });
const bought = (itemId, timestamp, participantId = 1) => ({ type: 'ITEM_PURCHASED', itemId, participantId, timestamp });
const ids = (b) => b.purchases.map((p) => p.itemId);

test('purchase order: an undo removes the latest earlier purchase of that item; sales change nothing', () => {
  const b = h.playerBuild(timeline([
    bought(1036, 70000), bought(1036, 80000),
    { type: 'ITEM_UNDO', participantId: 1, timestamp: 81000, beforeId: 1036, afterId: 0, goldGain: 350 },
    bought(3078, 900000),
    { type: 'ITEM_SOLD', participantId: 1, itemId: 3078, timestamp: 950000 },
    { type: 'ITEM_UNDO', participantId: 1, timestamp: 951000, beforeId: 0, afterId: 3078, goldGain: -2333 },
    { type: 'ITEM_DESTROYED', participantId: 1, itemId: 1036, timestamp: 900000 },
  ]), 'P', CATALOG);
  assert.deepEqual(b.purchases, [{ itemId: 1036, t: 70000 }, { itemId: 3078, t: 900000 }], 'the second Long Sword was undone; the sold Trinity Force stays');
  assert.deepEqual(b.items, [{ itemId: 3078, t: 900000 }]);
});

test("participantId 0 isn't a player, and only NORMAL level-ups count", () => {
  const b = h.playerBuild(timeline([
    bought(3865, 0, 0), bought(1055, 5000),
    { type: 'SKILL_LEVEL_UP', participantId: 1, skillSlot: 1, levelUpType: 'NORMAL', timestamp: 1000 },
    { type: 'SKILL_LEVEL_UP', participantId: 1, skillSlot: 2, levelUpType: 'EVOLVE', timestamp: 2000 },
    { type: 'SKILL_LEVEL_UP', participantId: 1, skillSlot: 3, levelUpType: 'NORMAL', timestamp: 3000 },
  ], [{ participantId: 0, puuid: 'P' }, { participantId: 1, puuid: 'P' }]), 'P', CATALOG);
  assert.deepEqual(ids(b), [1055], 'not the 3865 bought by participantId 0');
  assert.deepEqual(b.levelUps, [{ slot: 1, t: 1000 }, { slot: 3, t: 3000 }]);
  assert.equal(b.skills, 'QE');
  assert.equal(b.skillOrder, 'QEW', 'W never levelled ranks last');
});

test('the starter is cut at 60 s: two potions kept, the trinket dropped; a tag-less boots upgrade is boots', () => {
  assert.equal(h.BUILD_STARTER_MS, 60000);
  const b = h.playerBuild(timeline([bought(2003, 7000), bought(3340, 8000), bought(1055, 9000), bought(2003, 10000), bought(1001, 61000), bought(3172, 700000)]), 'P', CATALOG);
  assert.deepEqual(b.starter, [1055, 2003, 2003]);
  assert.equal(b.boots, 3172, 'Gunmetal Greaves, built from tagged boots; base Boots are not a choice');
});

test('skill order ranks by points, a tie going to the skill that got there first', () => {
  const up = (skillSlot, timestamp) => ({ type: 'SKILL_LEVEL_UP', participantId: 1, skillSlot, levelUpType: 'NORMAL', timestamp });
  const b = h.playerBuild(timeline([up(2, 1), up(1, 2), up(1, 3), up(2, 4), up(4, 5), up(3, 6)]), 'P', CATALOG);
  assert.equal(b.skillOrder, 'QWE', 'Q and W both 2: Q reached 2 first');
  assert.equal(b.skills, 'WQQWRE');
});

test('without item.json, purchases and skills are still worked out; starter, boots and items are null', () => {
  const b = h.playerBuild(timeline([bought(1055, 5000), { type: 'SKILL_LEVEL_UP', participantId: 1, skillSlot: 1, levelUpType: 'NORMAL', timestamp: 1 }]), 'P', null);
  assert.deepEqual([ids(b), b.skills, b.starter, b.boots, b.items], [[1055], 'Q', null, null, null]);
});

test('a strange timeline or an unknown player gives an empty build, never an error', () => {
  const empty = { purchases: [], starter: [], boots: null, items: [], skills: '', skillOrder: null, levelUps: [] };
  for (const t of [null, {}, { info: null }, { info: { participants: 'x', frames: 7 } }, { info: { participants: [{ participantId: 1, puuid: 'P' }], frames: [null, { events: [null, 3, {}] }] } }]) {
    assert.deepEqual(h.playerBuild(t, 'P', CATALOG), empty, JSON.stringify(t));
  }
  assert.deepEqual(h.playerBuild(timeline([bought(1055, 1)]), 'someone else', CATALOG), empty);
});

test('the golden file: every player of OC1_711969250, as builds:extract works it out', () => {
  const tl = BLD('OC1_711969250.timeline.json');
  const expected = BLD('OC1_711969250.expected.json');
  assert.equal(Object.keys(expected).length, 10);
  for (const [puuid, want] of Object.entries(expected)) {
    const b = h.playerBuild(tl, puuid, CATALOG);
    assert.deepEqual({ starter: b.starter, boots: b.boots, items: b.items.map((x) => x.itemId), skills: b.skills, skillOrder: b.skillOrder }, want, puuid);
  }
});

test('shop visits: purchases under 30 s apart are one visit', () => {
  assert.equal(h.SHOP_VISIT_GAP_MS, 30000);
  const p = (itemId, t) => ({ itemId, t });
  assert.deepEqual(h.shopVisits([p(1055, 5000), p(2003, 6000), p(3340, 34999), p(1036, 65000), p(3078, 900000)]), [
    { t: 5000, items: [1055, 2003, 3340] }, { t: 65000, items: [1036] }, { t: 900000, items: [3078] },
  ]);
  assert.deepEqual(h.shopVisits([]), []);
});

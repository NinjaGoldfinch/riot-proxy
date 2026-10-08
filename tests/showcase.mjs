// Unit tests for the showcase's pure helpers (DEV-06). They live in one marked
// block of src/ui/showcase.html, which has no build step, so this test cuts that
// block out and evaluates it. Run by `cargo test` (tests/ui.rs) when node is on PATH:
//   node --test tests/showcase.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../src/ui/showcase.html', import.meta.url), 'utf8');
const block = html.match(/\/\/ -{10} pure helpers[^\n]*\n([\s\S]*?)\/\/ -{10} end pure helpers/);
assert.ok(block, 'the pure helpers block is marked in showcase.html');
const h = new Function(`${block[1]}; return { APEX, QUEUES, LADDER_PAGE, tierColour, winRate, pct, sortLadder, ladderPage, championIndex, imageUrl, statusNotices, byChampion, topChampions, parseRiotId, parseRoute, explorerLink };`)();

test('apex tiers and queues are the ones the apex route accepts', () => {
  assert.deepEqual(h.APEX, ['CHALLENGER', 'GRANDMASTER', 'MASTER']);
  assert.deepEqual(h.QUEUES.map(([q]) => q), ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR']);
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

test('analytics rows per tier are summed per champion', () => {
  const rows = [
    { championId: 1, championName: 'Annie', tier: 'GOLD', games: 10, wins: 6 },
    { championId: 1, championName: 'Annie', tier: 'MASTER', games: 10, wins: 4 },
    { championId: 2, championName: 'Olaf', tier: 'GOLD', games: 4, wins: 3 },
  ];
  assert.deepEqual(h.byChampion(rows), [
    { championId: 1, championName: 'Annie', games: 20, wins: 10, winRate: 0.5 },
    { championId: 2, championName: 'Olaf', games: 4, wins: 3, winRate: 0.75 },
  ]);
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
});

test('source chips open the operation in the dev explorer', () => {
  assert.equal(h.explorerLink('GET /v1/lol/status/{platform}'), '/dev#explorer?op=GET%20%2Fv1%2Flol%2Fstatus%2F%7Bplatform%7D');
});

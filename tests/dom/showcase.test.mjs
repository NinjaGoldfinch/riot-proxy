// The showcase's home view (DEV-06) driven in jsdom: the real page, a fake
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

function api(calls, url, { status = {}, unauthorized = false } = {}) {
  const u = new URL(url, 'http://localhost');
  calls.push(`${u.pathname}${u.search}`);
  const p = u.pathname;
  if (p === '/dev/config.json') return [200, { authDisabled: !unauthorized, version: 'test', env: 'development', platforms: [{ value: 'oc1', label: 'Oceania', region: 'sea' }, { value: 'kr', label: 'Korea', region: 'asia' }], regions: ['sea', 'asia'] }];
  if (p === '/v1/static/versions') return [200, { current: '16.19.1', versions: ['16.19.1'] }];
  if (p === '/v1/static/champion') return [200, CHAMPS];
  if (unauthorized) return [401, { error: { code: 'UNAUTHORIZED', message: 'missing key', requestId: 'r' } }];
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
  assert.ok(p.text('#view').includes(`Player P${LADDER - 1} #OCE`));
  assert.ok(p.text('#view').includes('DEV-07'));
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

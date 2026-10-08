// The dev explorer's player tab (DEV-02, DEV-03) driven in jsdom: the real page,
// a fake same-origin API. Checks the wiring the pure-helper tests (tests/dev_ui.mjs)
// can't: paging calls, filters, the archive source, scoreboards, closing windows.
//   cd tests/dom && npm ci && npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const root = new URL('../../', import.meta.url);
const html = readFileSync(new URL('src/ui/dev-ui.html', root), 'utf8');
const match = JSON.parse(readFileSync(new URL('acceptance/fixtures/match.json', root), 'utf8'));
const PUUID = match.info.participants[0].puuid;
const LIVE_TOTAL = 60;
const ARCHIVED = 112;

const line = (i) => ({ matchId: `OC1_${1000 + i}`, queueId: 420, gameMode: 'CLASSIC', gameDuration: 1500, gameEndTimestamp: 1_760_000_000_000 - i * 1e6, player: { win: i % 3 !== 0, kills: i, deaths: 2, assists: 3, totalMinionsKilled: 150, neutralMinionsKilled: 10, championName: 'Viego', gameEndedInEarlySurrender: i === 7 } });

/** The proxy as the page sees it. Every request is logged as `METHOD path?query`. */
function api(calls, url, opts = {}) {
  const u = new URL(url, 'http://localhost');
  const m = opts.method ?? 'GET';
  calls.push(`${m} ${u.pathname}${u.search}`);
  const p = u.pathname;
  const q = (k) => u.searchParams.get(k);
  if (p === '/dev/config.json') return { authDisabled: true, version: 'test', env: 'development', platforms: [{ value: 'oc1', label: 'Oceania' }], regions: ['sea'] };
  if (p === '/dev/openapi.json') return { paths: {}, tags: [] };
  if (p.endsWith('/profile')) return { puuid: PUUID, platform: 'oc1', region: 'sea', account: { gameName: 'Tester', tagLine: 'OCE' }, summoner: { summonerLevel: 100 }, league: [], mastery: [], refreshAvailableIn: 0, ageSeconds: 1, warnings: [] };
  if (p === '/v1/static/champion') return { data: { Viego: { key: '234', name: 'Viego' } } };
  if (p === '/v1/static/queues') return [{ queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: null }, { queueId: 440, map: "Summoner's Rift", description: '5v5 Ranked Flex games', notes: null }, { queueId: 4, map: "Summoner's Rift", description: 'old solo', notes: 'Deprecated in favor of queueId 420' }];
  if (p === `/v1/players/${PUUID}/matches`) {
    const start = +q('start'), count = +q('count');
    const ms = Array.from({ length: Math.max(0, Math.min(count, LIVE_TOTAL - start)) }, (_, k) => line(start + k));
    return { puuid: PUUID, matches: ms, matchIds: ms.map((x) => x.matchId), hasMore: ms.length === count && start + count < LIVE_TOTAL, warnings: [], backfill: start === 0 ? { jobId: 'j', status: 'queued', limit: 500 } : null };
  }
  if (p === `/v1/admin/players/${PUUID}/archive/matches`) {
    const start = +q('start'), count = +q('count');
    const total = q('queue') === '440' ? 0 : ARCHIVED;
    const ms = Array.from({ length: Math.max(0, Math.min(count, total - start)) }, (_, k) => ({ matchId: `OC1_${5000 - start - k}`, queueId: 420, gameEndTimestamp: 1_700_000_000_000, gameDuration: 1800, remake: false, championId: 234, position: 'JUNGLE', win: true, kills: 5, deaths: 5, assists: 5, cs: 200 }));
    return { puuid: PUUID, total, start, count, matches: ms };
  }
  if (p === `/v1/admin/players/${PUUID}/archive`) return {
    puuid: PUUID, keyScope: 'k', player: { puuid: PUUID, tracked: false, lastSeenMatchId: 'OC1_1000', historyBackfillStartedAt: '2026-10-08T00:00:00Z', historyBackfilledAt: null, historyBackfillDepth: 200 },
    archive: { matches: ARCHIVED, remakes: 2, wins: 60, timelines: 0, oldestGameEnd: '2025-01-01T00:00:00Z', newestGameEnd: '2026-10-08T00:00:00Z', byQueue: [{ queueId: 420, matches: 100 }, { queueId: 440, matches: 12 }] },
    jobs: { 'archive:match': { pending: 1234, running: 1, done: 812, failed: 3 }, 'backfill:player': { pending: 0, running: 1, done: 0, failed: 0 } },
    latestJobs: [{ kind: 'backfill:player', state: 'running', payload: { puuid: PUUID, limit: 500 }, attempts: 1 }],
  };
  if (p.startsWith('/v1/lol/matches/')) return match;
  return { ok: true };
}

async function page() {
  const calls = [];
  const errors = [];
  const dom = new JSDOM(html.replace('<script type="module">', '<script>(async()=>{').replace(/<\/script>\s*<\/body>/, '})()</script></body>'), {
    url: 'http://localhost/dev#player', runScripts: 'dangerously', pretendToBeVisual: true,
    beforeParse(w) {
      w.fetch = async (url, opts) => {
        const body = JSON.stringify(api(calls, url, opts));
        return { status: 200, statusText: 'OK', headers: new Map([['x-cache', 'HIT'], ['content-type', 'application/json']]), text: async () => body, json: async () => JSON.parse(body) };
      };
      w.confirm = () => true;
      w.TextEncoder = TextEncoder;
      w.addEventListener('error', (e) => errors.push(e.message));
      w.addEventListener('unhandledrejection', (e) => errors.push(String(e.reason)));
    },
  });
  const w = dom.window;
  const $ = (s) => w.document.querySelector(s);
  const settle = async () => { for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 5)); };
  await settle();
  $('#plId').value = 'Tester#OCE';
  $('#plPlatform').value = 'oc1';
  $('#plForm').dispatchEvent(new w.Event('submit', { cancelable: true }));
  await settle();
  const choose = async (sel, value) => { const el = $(sel); el.value = value; el.dispatchEvent(new w.Event('change', { bubbles: true })); await settle(); };
  const click = async (sel) => { $(sel).click(); await settle(); };
  const esc = async () => { w.document.dispatchEvent(new w.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); await settle(); };
  const rows = () => w.document.querySelectorAll('#plMatches tr.match').length;
  const text = (sel) => $(sel)?.textContent ?? '';
  return { w, $, calls, errors, settle, choose, click, esc, rows, text };
}

test('live matches page by 10, 25 and 50 under the API cap of 20 per call', async () => {
  const p = await page();
  assert.equal(p.rows(), 10);
  assert.ok(p.text('#plMatches').includes('5v5 Ranked Solo games'), 'queue named from queues.json');
  assert.ok(!p.$('#plMatches [data-queue]').innerHTML.includes('old solo'), 'deprecated queue not offered');
  assert.ok(p.text('#plMatches').includes('win rate'));
  await p.choose('#plMatches [data-size]', '25');
  assert.equal(p.rows(), 25);
  assert.ok(p.calls.some((c) => c.endsWith('start=20&count=5')), 'a second call for 21–25');
  assert.equal(p.w.localStorage.getItem('rp.dev.pageSize'), '25');
  await p.click('#plMatches [data-page="1"]');
  assert.ok(p.text('#plMatches').includes('page 2 · 26–50'));
  await p.click('#plMatches [data-page="1"]');
  assert.equal(p.rows(), 10, 'the last page has what is left');
  assert.ok(p.$('#plMatches [data-page="1"]').disabled, 'Next stops at the end');
  await p.choose('#plMatches [data-queue]', '420');
  assert.ok(p.calls.at(-1).includes('queue=420'));
  assert.ok(p.text('#plMatches').includes('page 1'), 'a filter goes back to page 1');
  assert.deepEqual(p.errors, []);
});

test('the archive source pages through everything stored, with a total and no Riot calls', async () => {
  const p = await page();
  await p.click('#plOut [data-browse]');
  assert.ok(p.text('#plMatches').includes('Archived matches'));
  const first = p.calls.filter((c) => c.includes("/archive/matches")); assert.deepEqual(first, [`GET /v1/admin/players/${PUUID}/archive/matches?start=0&count=10`], "one archive call, page 1 at the remembered size");
  await p.choose('#plMatches [data-size]', '50');
  assert.equal(p.rows(), 50);
  assert.ok(p.text('#plMatches').includes(`page 1 of 3 · 1–50 of ${ARCHIVED}`));
  assert.ok(p.$('#plMatches [data-type]').disabled, 'the archive filters by queue only');
  await p.click('#plMatches [data-goto="2"]');
  assert.equal(p.rows(), ARCHIVED - 100);
  assert.ok(p.text('#plMatches').includes(`page 3 of 3 · 101–${ARCHIVED} of ${ARCHIVED}`));
  assert.ok(p.text('#plMatches').includes('Viego'), 'champion named from its id');
  await p.click('#plMatches [data-goto="0"]');
  assert.ok(p.text('#plMatches').includes('page 1 of 3'));
  await p.choose('#plMatches [data-queue]', '440');
  assert.ok(p.text('#plMatches').includes('nothing archived in this queue'));
  const live = p.calls.filter((c) => c.includes(`/v1/players/${PUUID}/matches`)).length;
  await p.choose('#plMatches [data-source]', 'riot');
  assert.equal(p.calls.filter((c) => c.includes(`/v1/players/${PUUID}/matches`)).length, live + 3, 'back to live: 50 is three calls');
  assert.deepEqual(p.errors, []);
});

test('the backfill card shows exact counts from the server', async () => {
  const p = await page();
  const card = p.text('#plArchive');
  for (const want of ['walking now · 200 ids deep', `${ARCHIVED} matches (2 remakes) · 60W 50L`, '5v5 Ranked Solo games: 100', '5v5 Ranked Flex games: 12', '1234 queued · 1 running · 812 done · 3 failed', `Browse all ${ARCHIVED} archived`]) {
    assert.ok(card.includes(want), `card has ${want}: ${card}`);
  }
  await p.click('#plOut [data-track="on"]');
  assert.ok(p.calls.includes('POST /v1/admin/tracked-players'));
  await p.click('#plOut [data-walk]');
  assert.ok(p.calls.includes('POST /v1/admin/backfill'));
  assert.deepEqual(p.errors, []);
});

test('scoreboards and response windows close with × and Esc', async () => {
  const p = await page();
  p.$('#plMatches tr.match td').click();
  await p.settle();
  assert.equal(p.w.document.querySelectorAll('#plMatches tr.detail .board table').length, 2, 'both teams');
  assert.ok(p.$('#plMatches tr.detail tr.me'), 'the player is marked');
  await p.click('#plMatches [data-close-match]');
  assert.equal(p.$('#plMatches tr.detail'), null);
  p.$('#plMatches tr.match td').click();
  await p.settle();
  await p.esc();
  assert.equal(p.$('#plMatches tr.detail'), null, 'Esc closes the scoreboard');
  await p.click('#plMatches [data-raw="matches"]');
  assert.ok(p.$('#plView [data-close]'));
  await p.click('#plView [data-close]');
  assert.equal(p.$('#plView').innerHTML, '');
  await p.click('#plMatches [data-raw="matches"]');
  await p.esc();
  assert.equal(p.$('#plView').innerHTML, '', 'Esc closes the viewer');
  assert.deepEqual(p.errors, []);
});

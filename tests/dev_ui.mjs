// Unit tests for the dev explorer's pure helpers (DEV-02). They live in one marked
// block of src/ui/dev-ui.html, which has no build step, so this test cuts that block
// out and evaluates it. Run by `cargo test` (tests/ui.rs) when node is on PATH:
//   node --test tests/dev_ui.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../src/ui/dev-ui.html', import.meta.url), 'utf8');
const block = html.match(/\/\/ -{10} pure helpers[^\n]*\n([\s\S]*?)\/\/ -{10} end pure helpers/);
assert.ok(block, 'the pure helpers block is marked in dev-ui.html');
const h = new Function(`${block[1]}; return { PAGE_SIZES, MATCH_CALL_MAX, pageCalls, mergePages, pageSummary, queueIndex, scoreboard, walkStatus, archivePage, pageCount, patchOf, clock, kdaRatio, kilo, role, RESET_WORD, resetArmed, resetTotals, probeVerdict, probeLists };`)();

test('page sizes are 10, 25 and 50', () => assert.deepEqual(h.PAGE_SIZES, [10, 25, 50]));

test('a page is split into calls of at most 20, the matches API limit', () => {
  assert.equal(h.MATCH_CALL_MAX, 20);
  assert.deepEqual(h.pageCalls(0, 10), [{ start: 0, count: 10 }]);
  assert.deepEqual(h.pageCalls(0, 25), [{ start: 0, count: 20 }, { start: 20, count: 5 }]);
  assert.deepEqual(h.pageCalls(1, 25), [{ start: 25, count: 20 }, { start: 45, count: 5 }]);
  assert.deepEqual(h.pageCalls(2, 50), [{ start: 100, count: 20 }, { start: 120, count: 20 }, { start: 140, count: 10 }]);
  for (const size of h.PAGE_SIZES) {
    for (let page = 0; page < 4; page++) {
      const calls = h.pageCalls(page, size);
      assert.equal(calls.reduce((n, c) => n + c.count, 0), size);
      assert.equal(calls[0].start, page * size);
      assert.ok(calls.every((c) => c.count >= 1 && c.count <= 20));
    }
  }
});

const ok = (json) => ({ status: 200, json });

test('merged pages keep call order and only have more when every call was full', () => {
  const m = h.mergePages([
    ok({ matches: [{ matchId: 'a' }], hasMore: true, warnings: ['w1'], backfill: { status: 'queued' } }),
    ok({ matches: [{ matchId: 'b' }], hasMore: true, warnings: [], backfill: null }),
  ]);
  assert.deepEqual(m.matches.map((x) => x.matchId), ['a', 'b']);
  assert.deepEqual(m.warnings, ['w1']);
  assert.equal(m.backfill.status, 'queued');
  assert.equal(m.hasMore, true);
  assert.equal(h.mergePages([ok({ matches: [], hasMore: true }), ok({ matches: [], hasMore: false })]).hasMore, false);
});

test('one failed call fails the page', () => {
  const bad = { status: 429, json: { error: { code: 'RATE_LIMITED' } } };
  assert.equal(h.mergePages([ok({ matches: [] }), bad]).failed, bad);
});

test('the summary leaves remakes out of the record and the averages', () => {
  const g = (win, k, d, a, cs, secs, remake = false) => ({ gameDuration: secs, player: { win, kills: k, deaths: d, assists: a, totalMinionsKilled: cs, neutralMinionsKilled: 0, gameEndedInEarlySurrender: remake } });
  const s = h.pageSummary([g(true, 10, 2, 5, 200, 1200), g(false, 2, 4, 3, 100, 600), g(false, 0, 0, 0, 5, 200, true)]);
  assert.equal(s.games, 3);
  assert.deepEqual([s.wins, s.losses, s.remakes], [1, 1, 1]);
  assert.equal(s.winRate, 50);
  assert.equal(s.kda, 3.33); // (12 + 8) / 6
  assert.equal(s.csPerMin, 10); // 300 cs / 30 min
  assert.equal(h.pageSummary([]).winRate, null);
  assert.equal(h.pageSummary([g(true, 3, 0, 1, 0, 60)]).kda, 4); // no deaths divides by 1
});

test('queues come from Riot queues.json; deprecated and unnamed ones are not offered', () => {
  const q = h.queueIndex([
    { queueId: 0, map: 'Custom games', description: null, notes: null },
    { queueId: 4, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: 'Deprecated in favor of queueId 420' },
    { queueId: 440, map: "Summoner's Rift", description: '5v5 Ranked Flex games', notes: null },
    { queueId: 420, map: "Summoner's Rift", description: '5v5 Ranked Solo games', notes: null },
  ]);
  assert.deepEqual(q.current.map((x) => x.queueId), [420, 440]);
  assert.equal(q.names[4], '5v5 Ranked Solo games'); // still labels old matches
  assert.equal(q.names[0], undefined);
  assert.deepEqual(h.queueIndex(null), { names: {}, current: [] });
});

test('the scoreboard splits a match-v5 body by team and marks the player', () => {
  const match = JSON.parse(readFileSync(new URL('../acceptance/fixtures/match.json', import.meta.url), 'utf8'));
  const me = match.info.participants[3].puuid;
  const teams = h.scoreboard(match, me);
  assert.equal(teams.length, match.info.teams.length);
  assert.equal(teams.reduce((n, t) => n + t.players.length, 0), match.info.participants.length);
  assert.ok(teams[0].teamId < teams[1].teamId);
  const marked = teams.flatMap((t) => t.players).filter((p) => p.me);
  assert.equal(marked.length, 1);
  const src = match.info.participants[3];
  assert.equal(marked[0].champion, src.championName);
  assert.equal(marked[0].cs, src.totalMinionsKilled + src.neutralMinionsKilled);
  for (const t of teams) assert.equal(t.win, Boolean(match.info.teams.find((x) => x.teamId === t.teamId).win));
  assert.deepEqual(h.scoreboard({}, me), []);
});

test('each team carries its side and totals for the header', () => {
  const match = JSON.parse(readFileSync(new URL('../acceptance/fixtures/match.json', import.meta.url), 'utf8'));
  const [blue, red] = h.scoreboard(match, '');
  assert.equal(blue.side, 'Blue side');
  assert.equal(red.side, 'Red side');
  assert.deepEqual(blue.totals, { kills: 30, deaths: 32, assists: 41, gold: 58053 });
  assert.deepEqual(red.totals, { kills: 31, deaths: 30, assists: 52, gold: 65833 });
  assert.equal(h.scoreboard({ info: { teams: [{ teamId: 300 }] } }, '')[0].side, 'team 300');
});

test('match details are formatted for reading', () => {
  assert.equal(h.patchOf('16.20.824.8524'), '16.20');
  assert.equal(h.patchOf(undefined), '');
  assert.equal(h.clock(1682), '28:02');
  assert.equal(h.clock(3725), '1:02:05');
  assert.equal(h.clock(0), '—');
  assert.equal(h.clock(undefined), '—');
  assert.equal(h.kdaRatio(0, 5, 7), '1.4');
  assert.equal(h.kdaRatio(3, 0, 4), 'Perfect');
  assert.equal(h.kdaRatio(0, 0, 0), '0.0');
  assert.equal(h.kilo(58053), '58.1k');
  assert.equal(h.kilo(950), '950');
  assert.deepEqual(['TOP', 'JUNGLE', 'MIDDLE', 'BOTTOM', 'UTILITY', '', 'Invalid'].map(h.role), ['Top', 'Jgl', 'Mid', 'Bot', 'Sup', '', 'Inv']);
});

test('walk status reads the player row and the walk job counts', () => {
  const row = { historyBackfillStartedAt: '2026-10-08T00:00:00Z', historyBackfilledAt: null, historyBackfillDepth: 300 };
  assert.equal(h.walkStatus(row, { running: 1, pending: 0 }).state, 'walking');
  assert.equal(h.walkStatus(row, { running: 0, pending: 1 }).state, 'queued');
  assert.deepEqual(h.walkStatus(row, { failed: 2 }), { state: 'stopped', depth: 300, at: row.historyBackfillStartedAt });
  assert.equal(h.walkStatus({ ...row, historyBackfilledAt: '2026-10-08T01:00:00Z' }, { done: 1 }).state, 'complete');
  assert.equal(h.walkStatus({ historyBackfillStartedAt: null }).state, 'never');
  assert.equal(h.walkStatus(null, {}).state, 'unknown');
});

test('an archive page reads like a live page, with a known total', () => {
  const res = { status: 200, json: { puuid: 'p', total: 3, start: 0, count: 2, matches: [
    { matchId: 'KR_2', queueId: 420, gameEndTimestamp: 2, gameDuration: 1800, remake: false, championId: 134, position: 'MIDDLE', win: true, kills: 8, deaths: 6, assists: 14, cs: 206 },
    { matchId: 'KR_1', queueId: 440, gameEndTimestamp: 1, gameDuration: null, remake: null, championId: 1, position: null, win: false, kills: null, deaths: null, assists: null, cs: null },
  ] } };
  const p = h.archivePage(res);
  assert.equal(p.total, 3);
  assert.equal(p.hasMore, true);
  assert.deepEqual(p.matches.map((m) => m.matchId), ['KR_2', 'KR_1']);
  assert.deepEqual(p.matches[0].player, { win: true, kills: 8, deaths: 6, assists: 14, totalMinionsKilled: 206, neutralMinionsKilled: 0, championId: 134, teamPosition: 'MIDDLE', gameEndedInEarlySurrender: false });
  assert.equal(p.matches[1].player.kills, 0);
  // The summary takes it as is; the game with no stored length stays out of CS/min.
  const s = h.pageSummary(p.matches);
  assert.deepEqual([s.wins, s.losses, s.csPerMin], [1, 1, 6.9]);
  assert.equal(h.archivePage({ ...res, json: { ...res.json, start: 1 } }).hasMore, false);
  const bad = { status: 403, json: { error: { code: 'FORBIDDEN' } } };
  assert.equal(h.archivePage(bad).failed, bad);
});

test('page count is at least one', () => {
  assert.equal(h.pageCount(0, 25), 1);
  assert.equal(h.pageCount(25, 25), 1);
  assert.equal(h.pageCount(26, 25), 2);
  assert.equal(h.pageCount(912, 50), 19);
});

test('the reset button arms only on the exact word', () => {
  assert.equal(h.RESET_WORD, 'reset');
  assert.equal(h.resetArmed('reset'), true);
  assert.equal(h.resetArmed('  reset '), true);
  for (const t of ['', 'Reset', 'RESET', 'rese', 'reset!', null, undefined]) assert.equal(h.resetArmed(t), false, String(t));
});

test('reset totals sum every table and list the non-empty ones, largest first', () => {
  const r = h.resetTotals([{ name: 'jobs', rows: 3 }, { name: 'players', rows: 0 }, { name: 'matches', rows: 40 }, { name: 'cache', rows: 3 }]);
  assert.equal(r.total, 46);
  assert.deepEqual(r.nonEmpty.map((t) => t.name), ['matches', 'cache', 'jobs']);
  assert.deepEqual(h.resetTotals([]), { total: 0, nonEmpty: [] });
  assert.deepEqual(h.resetTotals(), { total: 0, nonEmpty: [] });
});

test('probe verdicts flag what Riot no longer does', () => {
  assert.deepEqual(h.probeVerdict('confirmed'), ['ok', '✓ confirmed']);
  assert.equal(h.probeVerdict('not-seen')[0], 'muted');
  assert.equal(h.probeVerdict('changed')[0], 'no');
  assert.equal(h.probeVerdict('error')[0], 'no');
  assert.deepEqual(h.probeVerdict('new-one'), ['muted', 'new-one'], 'an unknown verdict is shown as is');
});

test('probe lists mark a full list and leave a failed one without counts', () => {
  const rows = h.probeLists([
    { tier: 'MASTER', entries: 10000, lowestLp: 313, capped: true, error: null },
    { tier: 'GRANDMASTER', entries: 700, lowestLp: 0, capped: false, error: null },
    { tier: 'CHALLENGER', entries: null, lowestLp: null, capped: false, error: 'UPSTREAM_ERROR' },
  ]);
  assert.deepEqual(rows[0], { tier: 'MASTER', players: '10,000', lowestLp: '313', note: 'at the cap: players below are not listed' });
  assert.deepEqual([rows[1].lowestLp, rows[1].note], ['0', '']);
  assert.deepEqual([rows[2].players, rows[2].lowestLp, rows[2].note], ['—', '—', 'error: UPSTREAM_ERROR']);
  assert.deepEqual(h.probeLists(), []);
});

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
const h = new Function(`${block[1]}; return { PAGE_SIZES, MATCH_CALL_MAX, pageCalls, mergePages, pageSummary, queueIndex, scoreboard, walkStatus };`)();

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

test('walk status reads the player row and the walk jobs', () => {
  const row = { historyBackfillStartedAt: '2026-10-08T00:00:00Z', historyBackfilledAt: null, historyBackfillDepth: 300 };
  assert.equal(h.walkStatus(row, [{ state: 'running' }]).state, 'walking');
  assert.equal(h.walkStatus(row, [{ state: 'pending' }]).state, 'queued');
  assert.deepEqual(h.walkStatus(row, [{ state: 'failed' }]), { state: 'stopped', depth: 300, at: row.historyBackfillStartedAt });
  assert.equal(h.walkStatus({ ...row, historyBackfilledAt: '2026-10-08T01:00:00Z' }, [{ state: 'done' }]).state, 'complete');
  assert.equal(h.walkStatus({ historyBackfillStartedAt: null }, []).state, 'never');
  assert.equal(h.walkStatus(undefined, []).state, 'unknown');
});

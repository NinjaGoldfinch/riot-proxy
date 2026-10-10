// The fake admin API the /dev/jobs suite (jobs.test.mjs, DEV-19) drives the page
// against. Shapes are the OpenAPI document's for `GET /v1/admin/jobs/activity`,
// `/v1/admin/jobs/{id}/activity`, `/v1/admin/jobs/queue`, `/v1/admin/jobs`,
// `/v1/admin/jobs/stats`, and the retry and cancel actions.
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('../../src/ui/jobs.html', import.meta.url), 'utf8');
const ago = (ms) => new Date(Date.now() - ms).toISOString();
const soon = (ms) => new Date(Date.now() + ms).toISOString();

const RUN = '01K00000000000000000000RUN';
const DONE = '01K0000000000000000000DONE';
const FAILED = '01K00000000000000000000BAD';
const PENDING = '01K0000000000000000000PEND';

const row = (id, kind, state, payload, extra = {}) => ({
  id, kind, dedupeKey: null, priority: 20_000, state, attempts: 1, runAfter: ago(60_000),
  claimedAt: state === 'running' ? ago(30_000) : null, finishedAt: null, error: null, payload, ...extra,
});
const walk = { crawlId: 'C1', platform: 'oc1', queue: 'RANKED_SOLO_5x5', tier: 'GOLD', division: 'II' };

function fake() {
  const jobs = {
    [RUN]: row(RUN, 'ladder:walk', 'running', walk),
    [DONE]: row(DONE, 'archive:match', 'done', { matchId: 'OC1_700001' }, { finishedAt: ago(5_000) }),
    [FAILED]: row(FAILED, 'archive:match', 'failed', { matchId: 'OC1_700002' }, { attempts: 5, finishedAt: ago(9_000), error: 'RIOT_UNAVAILABLE: match-v5 503' }),
    [PENDING]: row(PENDING, 'backfill:player', 'pending', { puuid: 'p'.repeat(78), platform: 'oc1', limit: 100 }, { runAfter: soon(30_000) }),
  };
  // The running walk logs one more page each time it is asked.
  const runEvents = [
    { seq: 0, at: ago(30_000), kind: 'job', text: 'claimed by worker 1 (attempt 1)', ms: null },
    { seq: 1, at: ago(29_000), kind: 'step', text: 'GOLD II page 1 on oc1', ms: null },
    { seq: 2, at: ago(28_500), kind: 'riot', text: 'league.entriesByTier /lol/league/v4/entries/RANKED_SOLO_5x5/GOLD/II → MISS', ms: 410 },
  ];
  const traces = {
    [RUN]: { kind: 'ladder:walk', worker: 1, attempt: 1, startedAt: ago(30_000), finishedAt: null, outcome: null, now: 'GOLD II page 1 on oc1', nowSince: ago(29_000), dropped: 0 },
    [DONE]: { kind: 'archive:match', worker: 2, attempt: 1, startedAt: ago(6_000), finishedAt: ago(5_000), outcome: 'done', now: null, nowSince: null, dropped: 0 },
  };
  const doneEvents = [
    { seq: 0, at: ago(6_000), kind: 'job', text: 'claimed by worker 2 (attempt 1)', ms: null },
    { seq: 1, at: ago(5_500), kind: 'wait', text: 'waited for the americas rate limit (match.byId)', ms: 300 },
    { seq: 2, at: ago(5_000), kind: 'job', text: 'done', ms: 1000 },
  ];
  const calls = [];

  function grow() {
    const page = runEvents.filter((e) => e.kind === 'step').length + 1;
    const text = `GOLD II page ${page} on oc1`;
    runEvents.push({ seq: runEvents.length, at: new Date().toISOString(), kind: 'step', text, ms: null });
    Object.assign(traces[RUN], { now: text, nowSince: new Date().toISOString() });
  }

  function api(url, opts) {
    const u = new URL(url, 'http://localhost');
    const p = u.pathname;
    calls.push({ method: opts.method, path: `${p}${u.search}` });
    if (p === '/dev/config.json') return [200, { authDisabled: true }];
    if (p === '/v1/admin/jobs/activity') {
      return [200, {
        workers: [
          { worker: 1, jobId: RUN, kind: 'ladder:walk', since: ago(30_000), now: traces[RUN].now, nowSince: traces[RUN].nowSince, events: runEvents.length },
          { worker: 2, jobId: null, kind: null, since: ago(5_000), now: null, nowSince: null, events: 0 },
        ],
        finished: [{ jobId: DONE, kind: 'archive:match', worker: 2, startedAt: ago(6_000), finishedAt: ago(5_000), outcome: 'done' }],
      }];
    }
    if (p === '/v1/admin/jobs/queue') {
      return [200, { running: [jobs[RUN]], next: [jobs[PENDING]], ready: 0, delayed: 1, nextDelayedAt: jobs[PENDING].runAfter, heldKinds: ['aggregate:analytics'], held: 2 }];
    }
    if (p === '/v1/admin/jobs' && u.searchParams.get('state') === 'failed') {
      return [200, { jobs: Object.values(jobs).filter((j) => j.state === 'failed') }];
    }
    if (p === '/v1/admin/jobs/stats') {
      return [200, {
        kinds: {
          'ladder:walk': { pending: 0, running: 1, done: 3, failed: 0 },
          'archive:match': { pending: 1200, running: 0, done: 50, failed: 1 },
          'names:backfill': { pending: 0, running: 0, done: 1, failed: 0 },
        },
        totals: { pending: 1200, running: 1, done: 54, failed: 1 },
      }];
    }
    let m = p.match(/^\/v1\/admin\/jobs\/(\w+)\/activity$/);
    if (m) {
      const id = m[1];
      if (!jobs[id]) return [404, { error: { code: 'NOT_FOUND', message: 'No such job' } }];
      const after = Number(u.searchParams.get('after') ?? 0);
      if (id === RUN && after > 0) grow();
      const events = id === RUN ? runEvents : id === DONE ? doneEvents : [];
      const t = traces[id];
      return [200, {
        job: jobs[id],
        trace: t ? { ...t, events: events.filter((e) => e.seq >= after), nextSeq: events.length } : null,
      }];
    }
    m = p.match(/^\/v1\/admin\/jobs\/(\w+)\/retry$/);
    if (m && opts.method === 'POST') {
      Object.assign(jobs[m[1]], { state: 'pending', attempts: 0, error: null, finishedAt: null, runAfter: new Date().toISOString() });
      return [200, jobs[m[1]]];
    }
    m = p.match(/^\/v1\/admin\/jobs\/(\w+)$/);
    if (m && opts.method === 'DELETE') {
      Object.assign(jobs[m[1]], { state: 'failed', error: 'cancelled', finishedAt: new Date().toISOString() });
      return [200, jobs[m[1]]];
    }
    return [503, { error: { code: 'UNAVAILABLE', message: 'not in this test' } }];
  }
  return { api, calls };
}

export { html, RUN, DONE, FAILED, PENDING, fake };

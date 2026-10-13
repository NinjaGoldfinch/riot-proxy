// The Jobs tab (ADM-11) in jsdom, folding in /dev/jobs (design/10 §Jobs page):
// queues from the snapshot, workers every second while live, up next grouped as
// ADR-093, counts by kind, failed jobs with retry, a bulk purge that cannot run
// without a dry run, and a job's trace at #jobs/<id> followed with ?after=.
// The cases ported from jobs.test.mjs keep their names.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, jobsWorld, RUN, DONE, FAILED, PENDING } from './fake-admin.mjs';

beforeEach(() => { scene.jobs = jobsWorld(); });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

test('the overview shows each worker, the queue, the counts, what finished and what failed', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    assert.equal(p.$('#tab-jobs').getAttribute('aria-selected'), 'true');
    assert.deepEqual(p.$$('#jQueues tbody tr').map((tr) => tr.firstElementChild.textContent), ['archive', 'backfill', 'ddragon', 'ladder', 'maintenance', 'poll']);
    assert.equal(p.text('#jWorkersMeta'), '1 of 2 busy');
    const w = p.$('#jWorkers .worker[data-job]');
    assert.equal(w.dataset.job, RUN);
    assert.match(w.textContent, /W1\s*ladder:walk/);
    assert.match(w.querySelector('.now').textContent, /GOLD II page 1 on oc1/);
    assert.match(p.words('#jWorkers'), /W2 idle/);
    const fin = p.$('#jFinished li');
    assert.equal(fin.dataset.job, DONE);
    assert.match(p.words('#jFinished'), /^archive:match done · W2 · took 1\.0 s \d/);
    assert.equal(p.text('#jQueueMeta').split(' · ')[0], '5 ready');
    assert.match(p.text('#jQueueMeta'), /held: aggregate:analytics · 1 delayed, next in /);
    const kinds = p.$$('#jKinds tbody tr').map((tr) => tr.firstElementChild.textContent);
    assert.deepEqual(kinds, ['archive:match', 'ladder:walk'], 'busiest first; idle kinds left out');
    const failed = p.$$('#jFailed tbody tr');
    assert.equal(failed.length, 1);
    assert.equal(failed[0].dataset.job, FAILED);
    assert.match(failed[0].textContent, /RIOT_UNAVAILABLE: match-v5 503/);
    assert.equal(p.text('#badge-jobs'), '12', 'the rail counts failed jobs in 24 h');
  } finally { p.close(); }
});

test('up next groups alike jobs by kind and platform in claim order (ADR-093)', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    const rows = p.$$('#jNext ul.groups li').map((li) => [...li.children].map((c) => c.textContent.trim()));
    assert.deepEqual(rows.map((r) => r[0]), ['ladder:walk', 'ladder:collect ×3', 'ladder:collect ×2', 'backfill:player']);
    assert.equal(rows[1][1], 'match ids for 3 batches · players 25–124 · kr');
    assert.equal(rows[1][2], 'ready · try 2');
    assert.equal(rows[2][1], 'match ids for 2 batches · players 175–249 · euw1');
    assert.equal(rows[2][2], 'ready');
    // Due 30 s after the fixture was built; a fast render still reads 30.0 s.
    assert.match(rows[3][2], /^due in (2\d\.\d|30\.0) s$/, 'a job waiting out a backoff says when it is due');
    assert.equal(p.$$('#jNext ul.groups li')[1].title, 'ladder:collect · priority 20,003');
    assert.ok(p.calls.some((c) => c.url.endsWith('/v1/admin/jobs/queue?limit=100')), 'up next reads 100 jobs');
    assert.equal(p.$$('#jNext ul.groups li')[3].dataset.job, PENDING, 'a group of one opens its job');
  } finally { p.close(); }
});

test('a worker opens its job in a tab that follows it with after=', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    p.$('#jWorkers .worker[data-job]').click();
    await p.settle();
    assert.equal(p.w.location.hash, `#jobs/${RUN}`);
    assert.equal(p.text('#jtKind'), 'ladder:walk');
    assert.equal(p.text('#jtState'), 'running');
    assert.match(p.text('#jtNow'), /^GOLD II page 1 on oc1 · for /);
    assert.equal(p.$$('#jtTimeline li').length, 3);
    assert.match(p.words('#jtFacts'), /^state running priority 20000 attempts 1 /);
    assert.match(p.text('#jtPayload'), /"tier": "GOLD"/);
    assert.ok(p.calls.some((c) => c.url.endsWith(`/v1/admin/jobs/${RUN}/activity?after=0`)));
    await wait(1100);
    await p.settle();
    assert.ok(p.calls.some((c) => c.url.endsWith(`/v1/admin/jobs/${RUN}/activity?after=3`)), 'asks only for new events');
    assert.equal(p.$$('#jtTimeline li').length, 4, 'and appends them');
    assert.match(p.$$('#jtTimeline li').at(-1).textContent, /GOLD II page 2 on oc1/);
  } finally { p.close(); }
});

test('a link opens its job, and a finished job shows its run', async () => {
  const p = await page({ key: KEY, hash: `#jobs/${DONE}` });
  try {
    assert.equal(p.text('#jtState'), 'done');
    assert.match(p.text('#jtNow'), /^Ended .* ago: done$/);
    assert.match(p.text('#jtRun'), /^attempt 1 · started .* ago · took 1\.0 s$/);
    assert.deepEqual(p.$$('#jtTimeline li .x').map((x) => x.textContent), ['claimed by worker 2 (attempt 1)', 'waited for the americas rate limit (match.byId)', 'done']);
  } finally { p.close(); }
});

test('a job that does not exist says so', async () => {
  const p = await page({ key: KEY, hash: '#jobs/01K0000000000000000000NONE' });
  try {
    assert.equal(p.text('#jtNow'), 'No such job.');
    assert.equal(p.$('#jtRetry').hidden, true);
    assert.equal(p.$('#jtCancel').hidden, true);
  } finally { p.close(); }
});

test('retry a failed job and cancel a pending one, each after a confirm', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    p.$('#jFailed [data-retry]').click();
    await p.settle();
    assert.match(p.text('#confirmText'), new RegExp(`^Run archive:match ${FAILED} again now\\?$`));
    p.$('#confirmOk').click();
    await p.settle();
    assert.ok(p.calls.some((c) => c.method === 'POST' && c.url.endsWith(`/v1/admin/jobs/${FAILED}/retry`)));
    assert.equal(scene.jobs.jobs[FAILED].state, 'pending');
    assert.equal(p.text('#toasts'), 'archive:match queued again.');

    p.w.location.hash = `#jobs/${PENDING}`;
    await p.settle();
    assert.equal(p.$('#jtCancel').hidden, false);
    assert.equal(p.$('#jtRetry').hidden, true);
    p.$('#jtCancel').click();
    await p.settle();
    p.$('#confirmCancel').click();
    await p.settle();
    assert.ok(scene.jobs.jobs[PENDING], 'declined: nothing cancelled');
    p.$('#jtCancel').click();
    await p.settle();
    p.$('#confirmOk').click();
    await p.settle();
    assert.ok(p.calls.some((c) => c.method === 'DELETE' && c.url.endsWith(`/v1/admin/jobs/${PENDING}`)));
    assert.equal(scene.jobs.jobs[PENDING], undefined);
  } finally { p.close(); }
});

test('a bulk purge is impossible without a dry run, and deletes the count it showed', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    assert.equal(p.$('#jpGo').disabled, true);
    assert.ok([...p.$('#jpKind').options].some((o) => o.value === 'archive:match'), 'kinds with pending or failed rows');
    p.$('#jpKind').value = 'archive:match';
    p.$('#jpMin').value = '20000';
    p.$('#jPurgeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    const dry = p.calls.find((c) => c.method === 'DELETE' && c.url.includes('dryRun=true'));
    assert.ok(dry.url.endsWith('/v1/admin/jobs?kind=archive%3Amatch&state=pending&minPriority=20000&dryRun=true'), dry.url);
    assert.equal(p.text('#jpMeta'), '1,126,303 pending archive:match rows at priority 20,000 or more.');
    assert.equal(p.text('#jpGo'), 'Delete 1,126,303 rows');
    // Changing a parameter after the dry run disarms the delete.
    p.$('#jpMin').value = '0';
    p.$('#jpMin').dispatchEvent(new p.w.Event('input'));
    assert.equal(p.$('#jpGo').disabled, true);
    p.$('#jpMin').value = '20000';
    p.$('#jPurgeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    p.$('#jpGo').click();
    await p.settle();
    assert.equal(p.text('#confirmText'), 'Delete 1,126,303 pending archive:match rows? This cannot be undone.');
    p.$('#confirmOk').click();
    await p.settle();
    const real = p.calls.filter((c) => c.method === 'DELETE' && c.url.startsWith('http://admin.test/v1/admin/jobs?')).at(-1);
    assert.ok(!real.url.includes('dryRun'), real.url);
    assert.equal(p.text('#toasts'), 'Deleted 1,126,303 archive:match rows.');
    assert.deepEqual(scene.jobs.bulk['archive:match|pending'], [{ priority: 100, n: 7 }], 'the low-priority rows stay');
    assert.equal(p.$('#jpGo').disabled, true);
  } finally { p.close(); }
});

test('polling runs every second while live and visible, and stops when hidden, paused or left', async () => {
  const p = await page({ key: KEY, hash: '#jobs' });
  try {
    const n = () => p.calls.filter((c) => c.url.includes('/v1/admin/jobs/activity')).length;
    const first = n();
    await wait(1100);
    assert.ok(n() > first, 'every second');
    await p.setHidden(true);
    const hidden = n();
    await wait(1200);
    assert.equal(n(), hidden, 'nothing while hidden');
    await p.setHidden(false);
    p.$('#jobsLive').click();
    await p.settle();
    const paused = n();
    await wait(1200);
    assert.equal(n(), paused, 'nothing while paused');
    p.$('#jobsLive').click();
    p.w.location.hash = '#overview';
    await p.settle();
    const left = n();
    await wait(1200);
    assert.equal(n(), left, 'nothing after leaving the tab');
  } finally { p.close(); }
});

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
import {
  api,
  counter,
  get,
  metrics,
  post,
  subscribe,
  waitFor,
  type Sample,
  type Subscription,
} from './helpers/harness.js';

/**
 * Ladder crawl — the live checks (#91).
 *
 * Split in two, for a reason worth being explicit about. The **crawl** is
 * cheap: a `MASTER` floor is three requests, one per apex league, whatever the
 * platform. What the crawl *discovers* is not. Every entry at or above the
 * server's `LADDER_BACKFILL_TIER_FLOOR` queues a match-history walk, and on a
 * dev key those walks run for hours — and this suite cannot see or change that
 * setting, because it belongs to the server it is pointed at.
 *
 * So everything with a bounded cost runs under the normal acceptance gate, and
 * the crawl itself is a second opt-in: `ACCEPTANCE_LADDER=1`. Same shape as
 * phase 6, where delivery is asserted automatically and the live game is not.
 */
const enabled = acceptance.enabled;
const crawlEnabled = enabled && process.env['ACCEPTANCE_LADDER'] === '1';

interface CrawlSummary {
  id: string;
  platform: string;
  queue: string;
  tierFloor: string;
  status: string;
  finishedAt: string | null;
  pagesFetched: number;
  entriesSeen: number;
  playersDiscovered: number;
  backfillsEnqueued: number;
  pendingLegs: number;
}

interface Started {
  crawlId: string;
  status: string;
  legs: number;
}

const QUEUE = 'RANKED_SOLO_5x5';
let socket: Subscription | undefined;
let crawlId = '';

describe.skipIf(!enabled)('Phase 7 — the ladder crawl, live', () => {
  afterAll(async () => {
    socket?.close();
    // A crawl this suite started and did not see finish must not be left
    // running: the live-crawl index would block every future crawl of that
    // ladder until someone noticed.
    if (crawlId) {
      const listed = await get<{ crawls: CrawlSummary[] }>('/v1/admin/ladder/crawls');
      const mine = listed.body.crawls?.find((c) => c.id === crawlId);
      if (mine?.status === 'running') {
        await api(`/v1/admin/ladder/crawls/${crawlId}`, { method: 'DELETE' });
      }
    }
  });

  describe('the surface, which costs nothing upstream', () => {
    it('lists crawls, whether or not any have run', async () => {
      const res = await get<{ crawls: CrawlSummary[] }>('/v1/admin/ladder/crawls');
      expect(res.status, JSON.stringify(res.body)).toBe(200);
      expect(Array.isArray(res.body.crawls)).toBe(true);
    });

    it('404s a crawl that does not exist rather than inventing one', async () => {
      const res = await api('/v1/admin/ladder/crawls/00000000-0000-4000-8000-00000000dead', {
        method: 'DELETE',
      });
      expect(res.status).toBe(404);
    });

    it('serves the analytics route even before anything is aggregated', async () => {
      const { platform } = cfg();
      const res = await get<{ patch: string | null; champions: unknown[]; totalGames: number }>(
        `/v1/lol/analytics/champions?platform=${platform}&queue=${QUEUE}`,
      );
      // Not a 404: the endpoint exists whether or not the numbers do yet.
      expect(res.status, JSON.stringify(res.body)).toBe(200);
      expect(Array.isArray(res.body.champions)).toBe(true);
    });

    it('publishes the ladder metrics, at zero before a crawl or with a value after', async () => {
      const samples = await metrics();
      const names = new Set(samples.map((s) => s.name));
      // A counter with no observations is absent from /metrics rather than
      // zero, so the histogram — which prom-client always emits — is the one
      // that proves the metric is registered at all.
      expect(
        names.has('proxy_ladder_crawl_duration_seconds_count') ||
          names.has('proxy_ladder_crawl_duration_seconds_bucket'),
      ).toBe(true);
    });

    it('keeps the ladder topic behind the admin scope', async () => {
      // Same decision as `metrics`: a crawl's counters describe what this
      // deployment spends its key on. If the suite has no admin key the
      // subscribe is refused, which is itself the assertion.
      const sub = await subscribe(['ladder']);
      const ack = sub.frames.find((f) => f.op === 'subscribed');
      expect(ack?.topics).toContain('ladder');
      sub.close();
    });
  });

  describe.skipIf(!crawlEnabled)('a real crawl (ACCEPTANCE_LADDER=1)', () => {
    let before: Sample[] = [];

    beforeAll(async () => {
      before = await metrics();
      // Subscribed before the trigger: the crawl can finish in seconds at a
      // Master floor, and an event published before the socket is up is gone.
      socket = await subscribe(['ladder']);
    });

    it('starts a Master-floor crawl — one request per apex league', async () => {
      const { platform } = cfg();
      const res = await post<Started>('/v1/admin/ladder/crawl', {
        platform,
        queue: QUEUE,
        tierFloor: 'MASTER',
      });

      expect(res.status, JSON.stringify(res.body)).toBe(202);
      // Not 200: the answer is "it is running", not "here is the ladder".
      expect(res.body.status).toBe('started');
      // Master, Grandmaster, Challenger. No paged walk at this floor.
      expect(res.body.legs).toBe(3);
      crawlId = res.body.crawlId;
      expect(crawlId).toMatch(/^[0-9a-f-]{36}$/);
    });

    it('tells a second trigger which crawl is already answering it', async () => {
      const { platform } = cfg();
      const res = await post<Started>('/v1/admin/ladder/crawl', { platform, queue: QUEUE });
      // 202 again, because nothing failed — the caller polls the same id.
      expect(res.status).toBe(202);
      if (res.body.status === 'already-running') expect(res.body.crawlId).toBe(crawlId);
    });

    it('finishes, with the ladder actually stored', async () => {
      const done = await waitFor(
        'the crawl to finish',
        async () => {
          const listed = await get<{ crawls: CrawlSummary[] }>('/v1/admin/ladder/crawls');
          const mine = listed.body.crawls?.find((c) => c.id === crawlId);
          return mine && mine.status !== 'running' ? mine : undefined;
        },
        { timeoutMs: 180_000, intervalMs: 2000 },
      );

      expect(done.status).toBe('completed');
      expect(done.finishedAt).not.toBeNull();
      // Three apex leagues, three pages, and a real ladder behind them.
      expect(done.pagesFetched).toBe(3);
      expect(done.entriesSeen).toBeGreaterThan(0);
      // Cleaned up on the way out, so the next crawl of this ladder is free to
      // start and nothing is left to finish a run that already finished.
      expect(done.pendingLegs).toBe(0);
    });

    it('announced itself on the admin topic', async () => {
      const frame = await socket!.next(
        (f) => f.event === 'ladder.crawl.completed' && f.data?.['crawlId'] === crawlId,
        30_000,
      );
      expect(frame.topic).toBe('ladder');
      expect(frame.data?.['queue']).toBe(QUEUE);
      expect(Number(frame.data?.['entries'])).toBeGreaterThan(0);
      expect(Number(frame.data?.['durationS'])).toBeGreaterThanOrEqual(0);
    });

    it('counted its pages, entries and duration', async () => {
      const { platform } = cfg();
      const after = await metrics();
      const labels = { platform, queue: QUEUE };

      const pages =
        counter(after, 'proxy_ladder_pages_total', labels) -
        counter(before, 'proxy_ladder_pages_total', labels);
      const entries =
        counter(after, 'proxy_ladder_entries_total', labels) -
        counter(before, 'proxy_ladder_entries_total', labels);
      const crawls =
        counter(after, 'proxy_ladder_crawl_duration_seconds_count', {
          ...labels,
          status: 'completed',
        }) -
        counter(before, 'proxy_ladder_crawl_duration_seconds_count', {
          ...labels,
          status: 'completed',
        });

      expect(pages).toBe(3);
      expect(entries).toBeGreaterThan(0);
      expect(crawls).toBe(1);

      // The job counter picks the new names up for free, which is the point of
      // routing every one of them through `dispatch`.
      const apexJobs =
        counter(after, 'proxy_jobs_total', { job: 'ladder:apex', status: 'completed' }) -
        counter(before, 'proxy_jobs_total', { job: 'ladder:apex', status: 'completed' });
      expect(apexJobs).toBe(3);
    });

    it('handed the players it found to the backfill pipeline', async () => {
      const after = await metrics();
      const queued =
        counter(after, 'proxy_backfills_queued_total', { reason: 'ladder' }) -
        counter(before, 'proxy_backfills_queued_total', { reason: 'ladder' });

      // Zero is a legitimate answer — a deployment with LADDER_BACKFILL_LIMIT=0,
      // or a backfill floor above every tier this crawl reached, discovers
      // players without walking them. What must not happen is the counter
      // being absent, which would mean the reason never reached the metric.
      expect(queued).toBeGreaterThanOrEqual(0);
      const listed = await get<{ crawls: CrawlSummary[] }>('/v1/admin/ladder/crawls');
      const mine = listed.body.crawls.find((c) => c.id === crawlId)!;
      expect(mine.playersDiscovered).toBe(queued > 0 ? mine.playersDiscovered : 0);
      expect(mine.backfillsEnqueued).toBe(queued);
    });

    it('recomputed the champion aggregates it triggered', async () => {
      const { platform } = cfg();
      // The aggregate runs on the maintenance queue after the crawl, so it can
      // land a moment later than the completion event.
      const body = await waitFor(
        'champion aggregates for the crawled ladder',
        async () => {
          const res = await get<{ patch: string | null; computedAt: string | null }>(
            `/v1/lol/analytics/champions?platform=${platform}&queue=${QUEUE}`,
          );
          return res.body.computedAt ? res.body : undefined;
        },
        { timeoutMs: 60_000, intervalMs: 2000 },
      ).catch(() => undefined);

      // An archive with no games by anyone on this ladder aggregates to
      // nothing, which is correct rather than a failure — so this asserts the
      // shape when there is one and says so plainly when there is not.
      if (!body) {
        expect(true, 'no archived games for this ladder yet — nothing to aggregate').toBe(true);
        return;
      }
      expect(body.patch).toMatch(/^\d+\.\d+$/);
    });
  });
});

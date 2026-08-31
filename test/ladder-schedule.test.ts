import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';

/**
 * `LADDER_CRAWL_S` is the switch between "this proxy crawls ladders" and "this
 * proxy can be asked to", and the default is off — a scheduled crawl discovers
 * players whose match histories are months of a dev key's budget (§2 of
 * docs/ladder-crawl-plan.md).
 *
 * Turning it back off has to *remove* the schedulers, not merely stop adding
 * them: BullMQ keeps a scheduler in Redis until something deletes it, so a
 * deployment that once ran with a cadence would keep firing against a config
 * saying it should not. That is the case worth a test.
 */
const upserted: { id: string; repeat: unknown; template: { data?: unknown } }[] = [];
/** Schedulers BullMQ is pretending to already hold. */
let existing = new Set<string>();

vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

vi.mock('bullmq', async (importOriginal) => {
  const actual = await importOriginal<typeof import('bullmq')>();
  class FakeQueue {
    async upsertJobScheduler(id: string, repeat: unknown, template: { data?: unknown }) {
      upserted.push({ id, repeat, template });
      existing.add(id);
      return {};
    }
    async removeJobScheduler(id: string) {
      return existing.delete(id);
    }
  }
  return { ...actual, Queue: FakeQueue };
});

const config = await import('../src/config.js');
const { scheduleLadderCrawls } = await import('../src/jobs/queues.js');
const { JOB } = await import('../src/jobs/queues.js');

/** The config object is frozen at import; these tests move one field at a time. */
function withConfig(over: Record<string, unknown>, run: () => Promise<void>) {
  const target = config.config as unknown as Record<string, unknown>;
  const before: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(over)) {
    before[k] = target[k];
    target[k] = v;
  }
  return run().finally(() => {
    for (const [k, v] of Object.entries(before)) target[k] = v;
  });
}

beforeEach(() => {
  upserted.length = 0;
  existing = new Set();
});

describe('the ladder crawl schedule', () => {
  it('schedules nothing at the default cadence of 0', async () => {
    expect(config.config.LADDER_CRAWL_S).toBe(0);
    await scheduleLadderCrawls();
    expect(upserted).toHaveLength(0);
  });

  it('removes a schedule left behind by a deployment that had one', async () => {
    const id = `${JOB.ladderCrawl.replaceAll(':', '-')}-euw1-RANKED_SOLO_5x5`;
    existing.add(id);

    await scheduleLadderCrawls();

    // The point: "off" has to mean the scheduler is gone, not merely that this
    // boot did not add it.
    expect(existing.has(id)).toBe(false);
  });

  it('schedules one crawl per (platform, queue), because a crawl is per ladder', async () => {
    await withConfig(
      {
        LADDER_CRAWL_S: 3600,
        ladderPlatforms: ['euw1', 'na1'],
        ladderQueues: ['RANKED_SOLO_5x5', 'RANKED_FLEX_SR'],
      },
      async () => {
        await scheduleLadderCrawls();
      },
    );

    expect(upserted).toHaveLength(4);
    expect(upserted.map((u) => u.template.data)).toEqual([
      { platform: 'euw1', queue: 'RANKED_SOLO_5x5' },
      { platform: 'euw1', queue: 'RANKED_FLEX_SR' },
      { platform: 'na1', queue: 'RANKED_SOLO_5x5' },
      { platform: 'na1', queue: 'RANKED_FLEX_SR' },
    ]);
    expect(new Set(upserted.map((u) => u.id)).size).toBe(4);
    expect(upserted[0]?.repeat).toEqual({ every: 3_600_000 });
  });

  it('keeps colons out of the scheduler id', async () => {
    // Each job a scheduler emits is named `repeat:<id>:<millis>`, and BullMQ
    // rejects a job id that splits into more than three parts — so an id
    // carrying the colons in `ladder:crawl` would fail at run time, not here.
    await withConfig({ LADDER_CRAWL_S: 60, ladderPlatforms: ['euw1'] }, async () => {
      await scheduleLadderCrawls();
    });
    expect(upserted[0]?.id).not.toContain(':');
  });
});

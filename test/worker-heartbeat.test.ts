import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { KEY_SCOPE } from '../src/config.js';
import {
  HEARTBEAT_TTL_MS,
  beat,
  startHeartbeat,
  stopHeartbeat,
  workerLiveness,
} from '../src/jobs/heartbeat.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * #80 — the snapshot could not tell a dead worker from an idle one. Both read
 * all-zero `active`, and the repeatable schedulers make it worse: their state
 * lives in Redis, so the tick jobs keep appearing whether or not anything runs
 * them. Only the worker can answer, so it says so itself.
 */
const KEY = `wrk:hb:${KEY_SCOPE}`;
let available = false;

beforeAll(async () => {
  available = await probeServices('worker-heartbeat.test.ts', async () => {
    await redis.ping();
    return true;
  });
});

beforeEach(async () => {
  if (available) await redis.del(KEY);
});

afterAll(async () => {
  if (available) await redis.del(KEY);
  await closeRedis();
});

describe('worker heartbeat (§13)', () => {
  it('reports no worker when none has ever beaten', async ({ skip }) => {
    if (!available) return skip();
    expect(await workerLiveness()).toEqual({ alive: false, lastSeenMs: null });
  });

  it('reports a live worker, and how long ago it was seen', async ({ skip }) => {
    if (!available) return skip();
    await beat();

    const live = await workerLiveness();
    expect(live.alive).toBe(true);
    // Derived from the key's own TTL, so this is Redis' clock throughout and
    // no agreement between the two processes is needed.
    expect(live.lastSeenMs).toBeGreaterThanOrEqual(0);
    expect(live.lastSeenMs).toBeLessThan(1000);
  });

  it('lets the key expire rather than holding a dead worker alive', async ({ skip }) => {
    if (!available) return skip();
    await beat();
    expect(await redis.pttl(KEY)).toBeLessThanOrEqual(HEARTBEAT_TTL_MS);

    // What a SIGKILL leaves: a beat that is nearly out of time and no process
    // to renew it. Reading must not extend it.
    await redis.pexpire(KEY, 40);
    await new Promise((r) => setTimeout(r, 80));
    expect(await workerLiveness()).toEqual({ alive: false, lastSeenMs: null });
  });

  it('drops the key on a deliberate shutdown, rather than waiting out the TTL', async ({
    skip,
  }) => {
    if (!available) return skip();
    const timer = startHeartbeat();
    // `startHeartbeat` beats immediately so a worker is up the moment it is.
    await new Promise((r) => setTimeout(r, 50));
    expect((await workerLiveness()).alive).toBe(true);

    await stopHeartbeat(timer);
    expect(await workerLiveness()).toEqual({ alive: false, lastSeenMs: null });
  });

  it('is scoped to this deployment, like every other key', async ({ skip }) => {
    if (!available) return skip();
    // A worker running against a different Riot key is a different deployment,
    // and must not answer for this one (§7.4).
    await redis.set('wrk:hb:0000dead', '1', 'PX', HEARTBEAT_TTL_MS);
    try {
      expect((await workerLiveness()).alive).toBe(false);
    } finally {
      await redis.del('wrk:hb:0000dead');
    }
  });
});

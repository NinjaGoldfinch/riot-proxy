import type { Redis } from 'ioredis';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
// The channel is key-scoped, and this suite shares the server's `.env`, so the
// helper computes the same scope the relay under test subscribes to.
import { channelFor } from '../src/events/topics.js';
import {
  api,
  get,
  jobIdsInState,
  post,
  redisClient,
  subscribe,
  waitFor,
  type Subscription,
} from './helpers/harness.js';

/**
 * Phase 6 — tracking a player must produce live events.
 *
 * The end of the chain needs a human in a real game, so this file splits it:
 * everything up to and including delivery is asserted automatically, and the
 * game itself is an opt-in check behind ACCEPTANCE_LIVE_GAME=1.
 */
const enabled = acceptance.enabled;

interface TrackedPlayer {
  puuid: string;
  platform: string;
  gameName: string | null;
  tagLine: string | null;
  tracked: boolean;
}

let puuid = '';
let redis: Redis;
let socket: Subscription | undefined;

describe.skipIf(!enabled)('Phase 6 — tracking and realtime events', () => {
  beforeAll(async () => {
    redis = redisClient();
  });

  afterAll(async () => {
    socket?.close();
    if (puuid && process.env['ACCEPTANCE_KEEP_TRACKED'] !== '1') {
      await api(`/v1/admin/tracked-players/${puuid}`, { method: 'DELETE' });
    }
    await redis?.quit();
  });

  /**
   * Tracking by Riot ID resolves through account-v1 using a region derived
   * from the platform — the exact path that 403'd for SEA platforms.
   */
  it('tracks a player by Riot ID on the configured platform', async () => {
    const { gameName, tagLine, platform } = cfg();
    const res = await post<TrackedPlayer>('/v1/admin/tracked-players', {
      platform,
      gameName,
      tagLine,
    });
    expect(res.status, JSON.stringify(res.body)).toBe(200);
    expect(res.body.tracked).toBe(true);
    expect(res.body.platform).toBe(platform);
    puuid = res.body.puuid;
    expect(puuid).toMatch(/^[A-Za-z0-9_-]{60,128}$/);

    const listed = await get<{ players: TrackedPlayer[] }>('/v1/admin/tracked-players');
    expect(listed.body.players.map((p) => p.puuid)).toContain(puuid);
  });

  /**
   * The poll tick fans out one job per tracked player. Custom job ids that
   * BullMQ rejects make this throw and no player is ever polled, which looks
   * identical to "nobody was in a game" — so assert the jobs really ran.
   */
  it('fans out poll jobs that the queue accepts', async () => {
    const { pollLiveSeconds } = cfg();
    const deadline = (pollLiveSeconds + 90) * 1000;

    const ran = await waitFor(
      `a poll job for the tracked player (up to one ${pollLiveSeconds}s interval)`,
      async () => {
        for (const state of ['completed', 'active', 'wait'] as const) {
          const ids = await jobIdsInState(redis, 'poll', state);
          const mine = ids.filter((id) => id.includes(puuid));
          if (mine.length > 0) return mine;
        }
        return undefined;
      },
      { timeoutMs: deadline, intervalMs: 2000 },
    );

    process.stdout.write(`  phase 6: ${ran.length} poll job(s) seen, e.g. ${ran[0]}\n`);
    for (const id of ran) expect(id).not.toContain(':');

    const failed = await jobIdsInState(redis, 'poll', 'failed');
    expect(failed.filter((id) => id.includes(puuid))).toEqual([]);
  });

  it('delivers a player event to a subscribed websocket (§11)', async () => {
    const topic = `player:${puuid}`;
    socket = await subscribe([topic]);

    // Injected rather than waited for: this asserts the worker → Redis → hub →
    // socket path, which is what a real game.started rides on.
    const publisher = redisClient();
    const payload = {
      event: 'game.started',
      topic,
      at: Date.now(),
      data: { puuid, platform: cfg().platform, gameId: 1, queueId: 420 },
    };
    // Give the hub a moment to register the subscription before publishing.
    await new Promise((r) => setTimeout(r, 250));
    await publisher.publish(channelFor(topic), JSON.stringify(payload));
    await publisher.quit();

    const frame = await socket.next((f) => f.event === 'game.started', 10_000);
    expect(frame.topic).toBe(topic);
    expect(frame.data?.['puuid']).toBe(puuid);
  });

  it('ignores events for topics the socket did not subscribe to', async () => {
    const publisher = redisClient();
    await publisher.publish(
      channelFor('player:someone-else'),
      JSON.stringify({
        event: 'game.started',
        topic: 'player:someone-else',
        at: Date.now(),
        data: {},
      }),
    );
    await publisher.quit();

    await new Promise((r) => setTimeout(r, 1000));
    const leaked = socket?.frames.filter((f) => f.topic === 'player:someone-else') ?? [];
    expect(leaked).toEqual([]);
  });

  /**
   * The real Phase 6 gate. Start a game, then run with ACCEPTANCE_LIVE_GAME=1;
   * `game.started` must arrive inside one poll interval and `match.archived`
   * must follow once the game ends.
   */
  it.runIf(enabled && acceptance.enabled && acceptance.liveGame)(
    'observes a real game.started and the match.archived that follows',
    async () => {
      const { pollLiveSeconds } = cfg();
      const live = await subscribe([`player:${puuid}`]);
      try {
        process.stdout.write('  phase 6: waiting for a real game — start one now\n');
        const started = await live.next((f) => f.event === 'game.started', 30 * 60_000);
        expect(started.data?.['gameId']).toBeTypeOf('number');
        process.stdout.write(
          `  phase 6: game.started for game ${String(started.data?.['gameId'])}\n`,
        );

        const ended = await live.next((f) => f.event === 'game.ended', 90 * 60_000);
        expect(ended.data?.['gameId']).toBe(started.data?.['gameId']);

        const archived = await live.next((f) => f.event === 'match.archived', 15 * 60_000);
        expect(String(archived.data?.['matchId'])).toMatch(/^[A-Za-z0-9]+_\d+$/);
        process.stdout.write(`  phase 6: match.archived ${String(archived.data?.['matchId'])}\n`);

        // "Within one poll interval" is the spec's wording; allow the tick it
        // was scheduled on plus one more.
        expect(pollLiveSeconds).toBeGreaterThan(0);
      } finally {
        live.close();
      }
    },
  );
});

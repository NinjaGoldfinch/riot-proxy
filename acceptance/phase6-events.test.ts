import { afterAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
import { MATCHES, matchId } from './helpers/mock-riot.js';
import {
  api,
  get,
  jobs,
  mockGame,
  post,
  subscribe,
  ULID,
  waitFor,
  type Subscription,
} from './helpers/harness.js';

/**
 * Phase 6 — tracking a player must produce live events.
 *
 * v1 injected a `game.started` through Redis to test delivery, and left the
 * real chain to a human in a real game. v2 has no Redis, and in mock mode it
 * needs neither: the mock Riot puts the player in a game, so every check here
 * rides the real chain — poll job, spectator diff, event, socket (ADR-059).
 * Live mode keeps v1's opt-in live game (ACCEPTANCE_LIVE_GAME=1).
 */
const enabled = acceptance.enabled;
const mock = acceptance.enabled && acceptance.mode === 'mock';

interface TrackedPlayer {
  puuid: string;
  platform: string;
  gameName: string | null;
  tagLine: string | null;
  tracked: boolean;
}

let puuid = '';
let otherPuuid = '';
let socket: Subscription | undefined;
let watcher: Subscription | undefined;

async function track(gameName: string, tagLine: string): Promise<TrackedPlayer> {
  const { platform } = cfg();
  const res = await post<TrackedPlayer>('/v1/admin/tracked-players', { platform, gameName, tagLine });
  expect(res.status, JSON.stringify(res.body)).toBe(200);
  return res.body;
}

describe.skipIf(!enabled)('Phase 6 — tracking and realtime events', () => {
  afterAll(async () => {
    socket?.close();
    watcher?.close();
    if (mock) await mockGame({ [puuid]: null, [otherPuuid]: null }).catch(() => {});
    if (process.env['ACCEPTANCE_KEEP_TRACKED'] !== '1') {
      for (const p of [puuid, otherPuuid].filter(Boolean)) {
        await api(`/v1/admin/tracked-players/${p}`, { method: 'DELETE' });
      }
    }
  });

  /**
   * Tracking by Riot ID resolves through account-v1 using a region derived
   * from the platform — the exact path that 403'd for SEA platforms.
   */
  it('tracks a player by Riot ID on the configured platform', async () => {
    const { gameName, tagLine, platform } = cfg();
    const body = await track(gameName, tagLine);
    expect(body.tracked).toBe(true);
    expect(body.platform).toBe(platform);
    puuid = body.puuid;
    expect(puuid).toMatch(/^[A-Za-z0-9_-]{60,128}$/);

    const listed = await get<{ players: TrackedPlayer[] }>('/v1/admin/tracked-players');
    expect(listed.body.players.map((p) => p.puuid)).toContain(puuid);
  });

  /**
   * The poll tick fans out one job per tracked player. A job the queue refuses
   * looks identical to "nobody was in a game", so assert the jobs really ran.
   * v1 checked BullMQ's custom job ids; v2's jobs are ULID rows, deduped by
   * PUUID (ADR-048).
   */
  it('fans out poll jobs that the queue accepts', async () => {
    const { pollLiveSeconds } = cfg();
    const ran = await waitFor(
      `a poll job for the tracked player (up to one ${pollLiveSeconds}s interval)`,
      async () => {
        const mine = (await jobs({ kind: 'poll:live' })).filter((j) => j.payload['puuid'] === puuid);
        return mine.some((j) => j.state === 'done') ? mine : undefined;
      },
      { timeoutMs: (pollLiveSeconds + 90) * 1000, intervalMs: 2000 },
    );

    process.stdout.write(`  phase 6: ${ran.length} poll job(s) seen, e.g. ${ran[0]!.id}\n`);
    for (const job of ran) {
      expect(job.id).toMatch(ULID);
      expect(job.dedupeKey).toBe(puuid);
    }
    expect(ran.filter((j) => j.state === 'failed')).toEqual([]);
  });

  /** The real chain: the player enters a game and the socket hears it. */
  it.runIf(mock)('delivers a player event to a subscribed websocket (§11)', async () => {
    const { pollLiveSeconds, other } = cfg();
    otherPuuid = (await track(other!.gameName, other!.tagLine)).puuid;
    const topic = `player:${puuid}`;
    socket = await subscribe([topic]);
    // Listening for the other player's event too, before either can happen:
    // the next check needs proof it was published.
    watcher = await subscribe([`player:${otherPuuid}`]);

    await mockGame({ [puuid]: 4242, [otherPuuid]: 4343 });
    const frame = await socket.next((f) => f.event === 'game.started', (pollLiveSeconds + 30) * 1000);
    expect(frame.topic).toBe(topic);
    expect(frame.data?.['puuid']).toBe(puuid);
    expect(frame.data?.['gameId']).toBe(4242);
  });

  /** The other player entered a game too: its event must not leak here. */
  it.runIf(mock)('ignores events for topics the socket did not subscribe to', async () => {
    const { pollLiveSeconds } = cfg();
    // Wait until the other player's event has demonstrably been published.
    await watcher!.next((f) => f.event === 'game.started', (pollLiveSeconds + 30) * 1000);
    const leaked = socket?.frames.filter((f) => f.topic === `player:${otherPuuid}`) ?? [];
    expect(leaked).toEqual([]);
  });

  /**
   * The real Phase 6 gate: `game.started` inside one poll interval, then
   * `game.ended` and the `match.archived` that follows. In mock mode the mock
   * plays the game; live, a human does (ACCEPTANCE_LIVE_GAME=1).
   */
  it.runIf(acceptance.enabled && acceptance.liveGame)(
    'observes a real game.started and the match.archived that follows',
    async () => {
      const { pollLiveSeconds } = cfg();
      const live = await subscribe([`player:${puuid}`]);
      try {
        let started;
        if (mock) {
          // Already in game since the previous check: the next poll says so
          // again without a new event, so start from that game's event.
          started = await socket!.next((f) => f.event === 'game.started', 5_000);
          await mockGame({ [puuid]: null });
        } else {
          process.stdout.write('  phase 6: waiting for a real game — start one now\n');
          started = await live.next((f) => f.event === 'game.started', 30 * 60_000);
        }
        expect(started.data?.['gameId']).toBeTypeOf('number');
        process.stdout.write(`  phase 6: game.started for game ${String(started.data?.['gameId'])}\n`);

        const ended = await live.next(
          (f) => f.event === 'game.ended',
          // A poll interval plus the spectator answer's cache life (5 s in mock mode).
          mock ? (pollLiveSeconds + 60) * 1000 : 90 * 60_000,
        );
        expect(ended.data?.['gameId']).toBe(started.data?.['gameId']);

        // The match the game produced, archived after the game ends. In mock
        // mode that is the match the mock added when the game ended; the
        // tracking backfill announces older ones meanwhile.
        const before = live.frames.length;
        const archived = await live.next(
          (f) =>
            f.event === 'match.archived' &&
            (mock ? f.data?.['matchId'] === matchId(MATCHES) : live.frames.indexOf(f) >= before),
          mock ? 5 * 60_000 : 15 * 60_000,
        );
        expect(String(archived.data?.['matchId'])).toMatch(/^[A-Za-z0-9]+_\d+$/);
        process.stdout.write(`  phase 6: match.archived ${String(archived.data?.['matchId'])}\n`);
        expect(pollLiveSeconds).toBeGreaterThan(0);
      } finally {
        live.close();
      }
    },
  );
});

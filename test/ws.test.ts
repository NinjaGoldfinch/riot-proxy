import './helpers/env.js';
import type { AddressInfo } from 'node:net';
import WebSocket from 'ws';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { buildApp, type App } from '../src/app.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { createTestConsumer, removeTestConsumers, testConsumerName } from './helpers/consumers.js';
import { publish } from '../src/events/index.js';
import { closeRedis, redis } from '../src/redis.js';
import { wsHub } from '../src/ws/index.js';

/** §11 — realtime layer, exercised over a real socket against a real server. */
let app: App | undefined;
let url = '';
let key = '';
let available = false;

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const consumer = await createTestConsumer({ name: testConsumerName('ws'), scopes: ['read'] });
  key = consumer?.key ?? '';

  app = await buildApp();
  await app.listen({ port: 0, host: '127.0.0.1' });
  const { port } = app.server.address() as AddressInfo;
  url = `ws://127.0.0.1:${port}/v1/ws`;
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  if (available) await removeTestConsumers();
  await Promise.allSettled([closeRedis(), closeDb()]);
});

/** Collect frames until `until` matches or the timeout expires. */
function collect(
  socket: WebSocket,
  until: (msgs: Record<string, unknown>[]) => boolean,
  timeoutMs = 4000,
): Promise<Record<string, unknown>[]> {
  return new Promise((resolve, reject) => {
    const msgs: Record<string, unknown>[] = [];
    const timer = setTimeout(
      () => reject(new Error(`timeout; got ${JSON.stringify(msgs)}`)),
      timeoutMs,
    );
    socket.on('message', (raw: Buffer) => {
      msgs.push(JSON.parse(raw.toString()) as Record<string, unknown>);
      if (until(msgs)) {
        clearTimeout(timer);
        resolve(msgs);
      }
    });
    socket.on('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

describe('websocket (§11)', () => {
  it('rejects an invalid key with a close code', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=rpx_definitelynotarealkey000000000`);
    const closeCode = await new Promise<number>((resolve) => socket.on('close', resolve));
    expect(closeCode).toBe(4401);
  });

  it('rejects a handshake with no token at all', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(url);
    const closeCode = await new Promise<number>((resolve) => socket.on('close', resolve));
    expect(closeCode).toBe(4401);
  });

  /**
   * Regression: auth awaits Redis/Postgres, so a client that subscribes the
   * instant the socket opens would have its first frame dropped unless the
   * server buffers messages arriving before auth completes.
   */
  it('honours a subscribe sent immediately on open', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=${key}`);
    const received = collect(socket, (m) => m.some((x) => x['op'] === 'subscribed'));
    socket.on('open', () =>
      socket.send(JSON.stringify({ op: 'subscribe', topics: ['player:P1'] })),
    );

    const msgs = await received;
    const subscribed = msgs.find((m) => m['op'] === 'subscribed');
    expect(subscribed?.['topics']).toEqual(['player:P1']);
    socket.close();
  });

  it('relays an event published on a subscribed topic', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=${key}`);
    await new Promise((r) => socket.once('open', r));

    const received = collect(socket, (m) => m.some((x) => x['event'] === 'game.started'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: ['player:P2'] }));
    // Let the subscription register before publishing.
    await new Promise((r) => setTimeout(r, 150));
    await publish('game.started', 'player:P2', {
      puuid: 'P2',
      platform: 'euw1',
      gameId: 42,
      championId: 266,
    });

    const msgs = await received;
    const event = msgs.find((m) => m['event'] === 'game.started');
    expect(event).toMatchObject({ topic: 'player:P2', data: { puuid: 'P2', gameId: 42 } });
    socket.close();
  });

  it('never delivers events for topics the client did not subscribe to', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=${key}`);
    await new Promise((r) => socket.once('open', r));

    const received = collect(socket, (m) => m.some((x) => x['event'] === 'patch.new'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: ['patch'] }));
    await new Promise((r) => setTimeout(r, 150));

    await publish('game.started', 'player:SOMEONE-ELSE', {
      puuid: 'SOMEONE-ELSE',
      platform: 'euw1',
      gameId: 1,
    });
    await publish('patch.new', 'patch', { version: '16.17.1' });

    const msgs = await received;
    expect(msgs.some((m) => m['topic'] === 'player:SOMEONE-ELSE')).toBe(false);
    socket.close();
  });

  it('supports unsubscribe and answers an application-level ping', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=${key}`);
    await new Promise((r) => socket.once('open', r));

    const received = collect(socket, (m) => m.some((x) => x['op'] === 'pong'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: ['a', 'b'] }));
    socket.send(JSON.stringify({ op: 'unsubscribe', topics: ['a'] }));
    socket.send(JSON.stringify({ op: 'ping' }));

    const msgs = await received;
    const last = msgs.filter((m) => m['op'] === 'subscribed').pop();
    expect(last?.['topics']).toEqual(['b']);
    expect(msgs.some((m) => m['op'] === 'pong')).toBe(true);
    socket.close();
  });

  it('rejects malformed frames and unknown ops without dropping the socket', async ({ skip }) => {
    if (!available) return skip();
    const socket = new WebSocket(`${url}?token=${key}`);
    await new Promise((r) => socket.once('open', r));

    const received = collect(socket, (m) => m.filter((x) => x['op'] === 'error').length >= 2);
    socket.send('not json at all');
    socket.send(JSON.stringify({ op: 'nonsense' }));

    const msgs = await received;
    const errors = msgs.filter((m) => m['op'] === 'error');
    expect(errors).toHaveLength(2);
    expect(socket.readyState).toBe(WebSocket.OPEN);
    socket.close();
  });
});

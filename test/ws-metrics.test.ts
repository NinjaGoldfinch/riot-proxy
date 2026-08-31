import './helpers/env.js';
import type { AddressInfo } from 'node:net';
import WebSocket from 'ws';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { buildApp, type App } from '../src/app.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { createTestConsumer, removeTestConsumers, testConsumerName } from './helpers/consumers.js';
import { EVENT_EXAMPLES } from '../src/events/examples.js';
import { FIREHOSE_TOPIC, METRICS_TOPIC, playerTopic, publish } from '../src/events/index.js';
import { closeRedis, redis } from '../src/redis.js';
import { METRICS_LOCK_KEY, metricsBroadcaster } from '../src/stats/broadcaster.js';
import { wsHub } from '../src/ws/index.js';

/**
 * The admin-only topics (§11): `metrics` and `firehose` are refused at
 * subscribe time for anything without the admin scope, which is what lets the
 * relay skip per-frame authorisation. Same harness as ws.test.ts — a real
 * server, real sockets, real Redis pub/sub.
 */
let app: App | undefined;
let url = '';
let readKey = '';
let adminKey = '';
let available = false;

beforeAll(async () => {
  available = await probeServices('ws-metrics.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (!available) return;

  const read = await createTestConsumer({ name: testConsumerName('wsm-read'), scopes: ['read'] });
  const admin = await createTestConsumer({
    name: testConsumerName('wsm-admin'),
    scopes: ['read', 'admin'],
  });
  readKey = read?.key ?? '';
  adminKey = admin?.key ?? '';

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

async function open(key: string): Promise<WebSocket> {
  const socket = new WebSocket(`${url}?token=${key}`);
  await new Promise((r) => socket.once('open', r));
  return socket;
}

/** Everything the frames deliver in `windowMs`, for the negative assertions. */
function within(socket: WebSocket, windowMs: number): Promise<Record<string, unknown>[]> {
  return new Promise((resolve) => {
    const msgs: Record<string, unknown>[] = [];
    socket.on('message', (raw: Buffer) => {
      msgs.push(JSON.parse(raw.toString()) as Record<string, unknown>);
    });
    setTimeout(() => resolve(msgs), windowMs);
  });
}

const SAMPLE = EVENT_EXAMPLES['metrics.snapshot'].data;

describe('admin-only topics (§11)', () => {
  it('refuses both admin topics to a read-scoped key, and says so per topic', async ({ skip }) => {
    if (!available) return skip();
    const socket = await open(readKey);

    const received = collect(
      socket,
      (m) =>
        m.filter((x) => x['op'] === 'error').length >= 2 && m.some((x) => x['op'] === 'subscribed'),
    );
    socket.send(
      JSON.stringify({ op: 'subscribe', topics: [METRICS_TOPIC, FIREHOSE_TOPIC, 'patch'] }),
    );

    const msgs = await received;
    const errors = msgs.filter((m) => m['op'] === 'error');
    expect(errors).toHaveLength(2);
    for (const error of errors) {
      expect((error['error'] as { code: string }).code).toBe('FORBIDDEN');
    }
    // The acknowledgement is honest: the allowed topic and nothing else.
    const subscribed = msgs.find((m) => m['op'] === 'subscribed');
    expect(subscribed?.['topics']).toEqual(['patch']);
    socket.close();
  });

  it('grants them to an admin key and delivers a published snapshot', async ({ skip }) => {
    if (!available) return skip();
    const socket = await open(adminKey);

    const acked = collect(socket, (m) => m.some((x) => x['op'] === 'subscribed'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: [METRICS_TOPIC, FIREHOSE_TOPIC] }));
    const ackMsgs = await acked;
    expect(ackMsgs.find((m) => m['op'] === 'subscribed')?.['topics']).toEqual([
      METRICS_TOPIC,
      FIREHOSE_TOPIC,
    ]);

    const received = collect(socket, (m) => m.some((x) => x['event'] === 'metrics.snapshot'));
    await new Promise((r) => setTimeout(r, 150));
    await publish('metrics.snapshot', METRICS_TOPIC, SAMPLE);

    const msgs = await received;
    const event = msgs.find((m) => m['event'] === 'metrics.snapshot');
    expect(event).toMatchObject({ topic: METRICS_TOPIC, data: { v: 1 } });
    socket.close();
  });

  it('mirrors a player event to the firehose without a player subscription', async ({ skip }) => {
    if (!available) return skip();
    const admin = await open(adminKey);
    const reader = await open(readKey);

    const adminAck = collect(admin, (m) => m.some((x) => x['op'] === 'subscribed'));
    admin.send(JSON.stringify({ op: 'subscribe', topics: [FIREHOSE_TOPIC] }));
    await adminAck;
    const readerAck = collect(reader, (m) => m.some((x) => x['op'] === 'subscribed'));
    reader.send(JSON.stringify({ op: 'subscribe', topics: ['patch'] }));
    await readerAck;

    const adminFrames = collect(admin, (m) => m.some((x) => x['event'] === 'match.archived'));
    const readerFrames = within(reader, 700);
    await new Promise((r) => setTimeout(r, 150));
    await publish('match.archived', playerTopic('WSM-FIRE'), {
      puuid: 'WSM-FIRE',
      matchId: 'EUW1_1',
    });

    const event = (await adminFrames).find((m) => m['event'] === 'match.archived');
    expect(event).toMatchObject({ topic: playerTopic('WSM-FIRE') });
    // The read-scoped socket holds `patch` only, and the firehose it was
    // refused must not leak the same event to it.
    expect((await readerFrames).some((m) => m['event'] === 'match.archived')).toBe(false);
    admin.close();
    reader.close();
  });
});

describe('the metrics broadcaster', () => {
  beforeEach(async () => {
    if (available) await redis.del(METRICS_LOCK_KEY);
  });

  it('publishes nothing while nobody is subscribed', async ({ skip }) => {
    if (!available) return skip();
    const socket = await open(adminKey);
    // Subscribed to the firehose, not to `metrics` — a produced snapshot would
    // reach this socket, so silence means the tick declined to build one.
    const acked = collect(socket, (m) => m.some((x) => x['op'] === 'subscribed'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: [FIREHOSE_TOPIC] }));
    await acked;

    const frames = within(socket, 600);
    await metricsBroadcaster.tick();
    expect((await frames).some((m) => m['event'] === 'metrics.snapshot')).toBe(false);
    socket.close();
  });

  it('skips a tick when another instance holds the lock', async ({ skip }) => {
    if (!available) return skip();
    const socket = await open(adminKey);
    const acked = collect(socket, (m) => m.some((x) => x['op'] === 'subscribed'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: [METRICS_TOPIC] }));
    await acked;

    await redis.set(METRICS_LOCK_KEY, '1', 'PX', 5000);
    const frames = within(socket, 600);
    await metricsBroadcaster.tick();
    expect((await frames).some((m) => m['event'] === 'metrics.snapshot')).toBe(false);
    socket.close();
  });

  it('ticks a real snapshot to a subscribed admin socket', async ({ skip }) => {
    if (!available) return skip();
    const socket = await open(adminKey);
    const acked = collect(socket, (m) => m.some((x) => x['op'] === 'subscribed'));
    socket.send(JSON.stringify({ op: 'subscribe', topics: [METRICS_TOPIC] }));
    await acked;
    await new Promise((r) => setTimeout(r, 150));

    const received = collect(socket, (m) => m.some((x) => x['event'] === 'metrics.snapshot'));
    await metricsBroadcaster.tick();

    const event = (await received).find((m) => m['event'] === 'metrics.snapshot');
    const data = event?.['data'] as { v: number; ws: { connections: number } };
    expect(data.v).toBe(1);
    expect(data.ws.connections).toBeGreaterThanOrEqual(1);
    socket.close();
  });
});

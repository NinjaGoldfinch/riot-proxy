import { Redis } from 'ioredis';
import WebSocket from 'ws';
import { cfg } from './env.js';

// ── HTTP ─────────────────────────────────────────────────────────────────────

export interface Response<T> {
  status: number;
  headers: Headers;
  body: T;
}

export async function api<T = unknown>(path: string, init: RequestInit = {}): Promise<Response<T>> {
  const { baseUrl, apiKey } = cfg();
  const headers = new Headers(init.headers);
  if (apiKey) headers.set('authorization', `Bearer ${apiKey}`);
  if (init.body && !headers.has('content-type')) headers.set('content-type', 'application/json');

  const res = await fetch(new URL(path, baseUrl), { ...init, headers });
  const text = await res.text();
  let body: T;
  try {
    body = text ? (JSON.parse(text) as T) : (undefined as T);
  } catch {
    body = text as T;
  }
  return { status: res.status, headers: res.headers, body };
}

/** Fail with the proxy's error envelope rather than a bare status code. */
export async function get<T>(path: string): Promise<Response<T>> {
  const res = await api<T>(path);
  if (res.status !== 200) {
    throw new Error(`GET ${path} -> ${res.status} ${JSON.stringify(res.body)}`);
  }
  return res;
}

export function post<T = unknown>(path: string, body: unknown): Promise<Response<T>> {
  return api<T>(path, { method: 'POST', body: JSON.stringify(body) });
}

/** The debug passthrough builds its request verbatim — use it to pin a method. */
export function passthrough<T = unknown>(
  scope: string,
  path: string,
  method: string,
  extra: Record<string, string> = {},
): Promise<Response<T>> {
  const qs = new URLSearchParams({ scope, path, method, ...extra });
  return api<T>(`/v1/admin/debug/riot?${qs.toString()}`);
}

// ── Prometheus ───────────────────────────────────────────────────────────────

export interface Sample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

const SAMPLE = /^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{(.*)\})?\s+(.+)$/;

export async function metrics(): Promise<Sample[]> {
  const { baseUrl } = cfg();
  const res = await fetch(new URL('/metrics', baseUrl));
  const text = await res.text();
  const samples: Sample[] = [];

  for (const line of text.split('\n')) {
    if (!line || line.startsWith('#')) continue;
    const match = SAMPLE.exec(line);
    if (!match) continue;
    const [, name = '', rawLabels = '', rawValue = ''] = match;
    const value = Number(rawValue);
    if (!Number.isFinite(value)) continue;

    const labels: Record<string, string> = {};
    for (const pair of rawLabels.matchAll(/([a-zA-Z_][a-zA-Z0-9_]*)="([^"]*)"/g)) {
      const [, key = '', val = ''] = pair;
      labels[key] = val;
    }
    samples.push({ name, labels, value });
  }
  return samples;
}

function matches(sample: Sample, labels: Record<string, string>): boolean {
  return Object.entries(labels).every(([k, v]) => sample.labels[k] === v);
}

/** Summed counter value; absent series read as 0, which is what a delta wants. */
export function counter(
  samples: Sample[],
  name: string,
  labels: Record<string, string> = {},
): number {
  return samples
    .filter((s) => s.name === name && matches(s, labels))
    .reduce((sum, s) => sum + s.value, 0);
}

/**
 * Prometheus-style p95: the first bucket whose cumulative count crosses 95% of
 * the total. Taken as a delta so a long-lived process's history cannot mask a
 * regression in the window under test.
 */
export function histogramP95(
  before: Sample[],
  after: Sample[],
  name: string,
  labels: Record<string, string> = {},
): { p95: number; count: number } {
  const bucketName = `${name}_bucket`;
  const total = counter(after, `${name}_count`, labels) - counter(before, `${name}_count`, labels);
  if (total <= 0) return { p95: 0, count: 0 };

  const les = [
    ...new Set(
      after
        .filter((s) => s.name === bucketName && matches(s, labels))
        .map((s) => s.labels['le'] ?? ''),
    ),
  ]
    .filter(Boolean)
    .sort((a, b) => Number(a) - Number(b));

  for (const le of les) {
    const delta =
      counter(after, bucketName, { ...labels, le }) -
      counter(before, bucketName, { ...labels, le });
    if (delta >= total * 0.95) return { p95: Number(le), count: total };
  }
  return { p95: Infinity, count: total };
}

// ── waiting ──────────────────────────────────────────────────────────────────

export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function waitFor<T>(
  label: string,
  probe: () => Promise<T | undefined>,
  opts: { timeoutMs: number; intervalMs?: number },
): Promise<T> {
  const interval = opts.intervalMs ?? 1000;
  const deadline = Date.now() + opts.timeoutMs;
  let last: unknown;
  while (Date.now() < deadline) {
    try {
      const result = await probe();
      if (result !== undefined) return result;
    } catch (err) {
      last = err;
    }
    await sleep(interval);
  }
  throw new Error(
    `timed out after ${opts.timeoutMs}ms waiting for ${label}` +
      (last ? ` (last error: ${String(last)})` : ''),
  );
}

/** Sorted latency samples in ms, so callers can take their own percentile. */
export function percentile(samples: number[], p: number): number {
  if (samples.length === 0) return 0;
  const sorted = [...samples].sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[index] ?? 0;
}

export async function timed(path: string): Promise<number> {
  const started = performance.now();
  await api(path);
  return performance.now() - started;
}

/** Bounded-concurrency map — `Promise.all` over 500 requests is not a test. */
export async function pool<T>(
  count: number,
  concurrency: number,
  task: (index: number) => Promise<T>,
): Promise<T[]> {
  const results = new Array<T>(count);
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, count) }, async () => {
    for (let i = next++; i < count; i = next++) results[i] = await task(i);
  });
  await Promise.all(workers);
  return results;
}

// ── websocket ────────────────────────────────────────────────────────────────

export interface ProxyEventFrame {
  event?: string;
  topic?: string;
  at?: number;
  data?: Record<string, unknown>;
  op?: string;
  topics?: string[];
}

export interface Subscription {
  frames: ProxyEventFrame[];
  /** Resolves with the first frame matching `predicate`, or rejects on timeout. */
  next(predicate: (f: ProxyEventFrame) => boolean, timeoutMs: number): Promise<ProxyEventFrame>;
  close(): void;
}

export async function subscribe(topics: string[]): Promise<Subscription> {
  const { wsUrl, apiKey } = cfg();
  const url = apiKey ? `${wsUrl}?token=${encodeURIComponent(apiKey)}` : wsUrl;
  const socket = new WebSocket(url);
  const frames: ProxyEventFrame[] = [];

  socket.on('message', (raw: Buffer) => {
    try {
      frames.push(JSON.parse(raw.toString()) as ProxyEventFrame);
    } catch {
      // A non-JSON frame is a protocol violation the tests assert on elsewhere.
    }
  });

  await new Promise<void>((resolve, reject) => {
    socket.once('open', () => resolve());
    socket.once('error', reject);
  });
  socket.send(JSON.stringify({ op: 'subscribe', topics }));

  const sub: Subscription = {
    frames,
    async next(predicate, timeoutMs) {
      return waitFor('a matching websocket frame', async () => frames.find(predicate), {
        timeoutMs,
        intervalMs: 250,
      });
    },
    close: () => socket.close(),
  };

  // Confirm the server acknowledged before any test relies on delivery.
  await sub.next((f) => f.op === 'subscribed', 5000);
  return sub;
}

// ── redis ────────────────────────────────────────────────────────────────────

export function redisClient(): Redis {
  return new Redis(cfg().redisUrl, { maxRetriesPerRequest: null, lazyConnect: false });
}

/**
 * BullMQ dedupes on custom job id and keeps completed/failed jobs around, so a
 * re-run of the same backfill is silently dropped. Clear the key first.
 */
export async function forgetJob(redis: Redis, queue: string, jobId: string): Promise<void> {
  await redis.del(`bull:${queue}:${jobId}`);
  await redis.zrem(`bull:${queue}:failed`, jobId);
  await redis.zrem(`bull:${queue}:completed`, jobId);
}

export async function jobIdsInState(
  redis: Redis,
  queue: string,
  state: 'wait' | 'active' | 'failed' | 'completed' | 'delayed',
): Promise<string[]> {
  const key = `bull:${queue}:${state}`;
  const type = await redis.type(key);
  if (type === 'list') return redis.lrange(key, 0, -1);
  if (type === 'zset') return redis.zrange(key, 0, -1);
  return [];
}

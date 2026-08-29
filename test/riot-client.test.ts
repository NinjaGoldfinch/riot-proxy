import './helpers/env.js';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { Pool } from 'undici';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { RiotError } from '../src/errors.js';
import { registry } from '../src/metrics.js';
import { RiotClient } from '../src/riot/client.js';
import { build } from '../src/riot/endpoints.js';
import type { RateLimiter } from '../src/riot/limiter.js';

/**
 * The §5.5 status policy is tested against a real HTTP server rather than a
 * mock: retries, backoff and body draining are exactly the behaviours a mock
 * would paper over.
 */

interface Handler {
  status: number;
  body?: unknown;
  headers?: Record<string, string>;
}

let server: Server;
let origin: string;
let queue: Handler[] = [];
let hits = 0;

beforeAll(async () => {
  server = createServer((req, res) => {
    hits += 1;
    const next = queue.shift() ?? { status: 200, body: { ok: true } };
    res.writeHead(next.status, {
      'content-type': 'application/json',
      ...(next.headers ?? {}),
    });
    res.end(JSON.stringify(next.body ?? {}));
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  origin = `http://127.0.0.1:${port}`;
});

afterAll(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

beforeEach(() => {
  queue = [];
  hits = 0;
});

/** A limiter stub: acquisition always succeeds, and calls are observable. */
function stubLimiter() {
  return {
    acquire: vi.fn(async () => ({ waitedMs: 0 })),
    observeHeaders: vi.fn(async () => undefined),
    freeze: vi.fn(async () => undefined),
    isFrozen: vi.fn(async () => 0),
    usage: vi.fn(async () => []),
    clearLocalConfig: vi.fn(),
  } as unknown as RateLimiter & {
    acquire: ReturnType<typeof vi.fn>;
    freeze: ReturnType<typeof vi.fn>;
    observeHeaders: ReturnType<typeof vi.fn>;
  };
}

function makeClient(limiter = stubLimiter()) {
  const client = new RiotClient({
    rateLimiter: limiter,
    poolFactory: () => new Pool(origin),
  });
  return { client, limiter };
}

const REQ = build.summonerByPuuid('euw1', 'PUUID');

describe('riot client (§5.2, §5.5)', () => {
  it('returns the parsed body on 200 and reports latency', async () => {
    queue.push({ status: 200, body: { summonerLevel: 500 } });
    const { client } = makeClient();
    const res = await client.request<{ summonerLevel: number }>(REQ);

    expect(res.data).toEqual({ summonerLevel: 500 });
    expect(res.status).toBe(200);
    expect(res.upstreamMs).toBeGreaterThanOrEqual(0);
    await client.close();
  });

  it('acquires from the limiter before every dispatch (§9.2)', async () => {
    queue.push({ status: 200, body: {} });
    const { client, limiter } = makeClient();
    await client.request(REQ);

    expect(limiter.acquire).toHaveBeenCalledWith(
      'euw1',
      'summoner.byPuuid',
      expect.objectContaining({ priority: 'interactive' }),
    );
    await client.close();
  });

  it('feeds every response back into the limiter, errors included (§9.1)', async () => {
    queue.push({ status: 200, body: {}, headers: { 'x-app-rate-limit': '20:1' } });
    const { client, limiter } = makeClient();
    await client.request(REQ);
    // observeHeaders is fire-and-forget; let the microtask land.
    await new Promise((r) => setTimeout(r, 10));

    expect(limiter.observeHeaders).toHaveBeenCalledWith(
      'euw1',
      'summoner.byPuuid',
      expect.objectContaining({ 'x-app-rate-limit': '20:1' }),
    );
    await client.close();
  });

  it('throws a typed 404 without retrying', async () => {
    queue.push({ status: 404 });
    const { client } = makeClient();

    await expect(client.request(REQ)).rejects.toMatchObject({ name: 'RiotError', status: 404 });
    expect(hits).toBe(1);
    await client.close();
  });

  it('never retries a 401/403 — the key is bad, not the network (§5.5)', async () => {
    for (const status of [401, 403]) {
      queue = [{ status }];
      hits = 0;
      const { client } = makeClient();
      const err = (await client.request(REQ).catch((e: unknown) => e)) as RiotError;

      expect(err).toBeInstanceOf(RiotError);
      expect(err.status).toBe(status);
      expect(hits).toBe(1);
      await client.close();
    }
  });

  it('retries a 5xx twice then gives up (§5.5)', async () => {
    queue.push({ status: 500 }, { status: 502 }, { status: 503 });
    const { client } = makeClient();

    await expect(client.request(REQ)).rejects.toMatchObject({ name: 'RiotError' });
    expect(hits).toBe(3); // original + 2 retries
    await client.close();
  });

  it('recovers when a retried 5xx succeeds', async () => {
    queue.push({ status: 500 }, { status: 200, body: { recovered: true } });
    const { client } = makeClient();

    await expect(client.request(REQ)).resolves.toMatchObject({ data: { recovered: true } });
    expect(hits).toBe(2);
    await client.close();
  });

  it('freezes the scope on a typed 429 with Retry-After (§9.4)', async () => {
    queue.push(
      { status: 429, headers: { 'x-rate-limit-type': 'application', 'retry-after': '2' } },
      { status: 200, body: { ok: true } },
    );
    const { client, limiter } = makeClient();

    await expect(client.request(REQ)).resolves.toMatchObject({ data: { ok: true } });
    expect(limiter.freeze).toHaveBeenCalledWith('euw1', 2, 'application');
    await client.close();
  });

  it('counts each 429 exactly once (§13)', async () => {
    // The counter was incremented both here and inside `limiter.freeze`, so
    // accountable 429s — the only types that freeze — read double.
    const read = async (type: string) => {
      const metric = registry.getSingleMetric('proxy_rl_429_total');
      const collected = await metric?.get();
      return (
        collected?.values.find((v) => v.labels['region'] === 'euw1' && v.labels['type'] === type)
          ?.value ?? 0
      );
    };

    const before = await read('application');
    queue.push(
      { status: 429, headers: { 'x-rate-limit-type': 'application', 'retry-after': '1' } },
      { status: 200, body: { ok: true } },
    );
    const { client } = makeClient();
    await expect(client.request(REQ)).resolves.toMatchObject({ data: { ok: true } });

    expect((await read('application')) - before).toBe(1);
    await client.close();
  });

  it('backs off without touching buckets on an untyped 429 (§5.4 rule 4)', async () => {
    queue.push({ status: 429 }, { status: 200, body: { ok: true } });
    const { client, limiter } = makeClient();

    await expect(client.request(REQ)).resolves.toMatchObject({ data: { ok: true } });
    // No type header means our accounting was fine — the scope must not freeze.
    expect(limiter.freeze).not.toHaveBeenCalled();
    expect(hits).toBe(2);
    await client.close();
  });

  it('gives up on a persistent untyped 429 after the documented tries', async () => {
    queue.push({ status: 429 }, { status: 429 }, { status: 429 }, { status: 429 });
    const { client } = makeClient();

    await expect(client.request(REQ)).rejects.toMatchObject({ status: 429 });
    expect(hits).toBe(4); // original + 3 backoff retries
    await client.close();
  }, 30_000);
});

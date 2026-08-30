import { randomUUID } from 'node:crypto';
import type { Redis } from 'ioredis';
import { config, KEY_SCOPE } from '../config.js';
import { logger } from '../logger.js';
import { rlWaitSeconds } from '../metrics.js';
import { redis as defaultRedis } from '../redis.js';
import { ACQUIRE_SCRIPT, SYNC_SCRIPT } from './limiter-scripts.js';

export type Priority = 'interactive' | 'bulk';

/** One Riot limit window, e.g. `20:1` → 20 requests per 1 second. */
export interface LimitWindow {
  limit: number;
  seconds: number;
}

/**
 * §2.3 / §9.1 — used only until the first response teaches us the real limits.
 * These are the documented development-key values; production keys are higher
 * and the limiter re-configures itself from headers on every response.
 */
export const BOOTSTRAP_APP_LIMITS: LimitWindow[] = [
  { limit: 20, seconds: 1 },
  { limit: 100, seconds: 120 },
];

/** Parse Riot's `20:1,100:120` header format. */
export function parseLimitHeader(header: string | undefined): LimitWindow[] {
  if (!header) return [];
  const out: LimitWindow[] = [];
  for (const part of header.split(',')) {
    const [limitRaw, secondsRaw] = part.trim().split(':');
    const limit = Number(limitRaw);
    const seconds = Number(secondsRaw);
    if (Number.isFinite(limit) && Number.isFinite(seconds) && limit > 0 && seconds > 0) {
      out.push({ limit, seconds });
    }
  }
  return out;
}

const cfgKey = {
  app: (scope: string) => `rl:cfg:app:${KEY_SCOPE}:${scope}`,
  method: (scope: string, method: string) => `rl:cfg:m:${KEY_SCOPE}:${scope}:${method}`,
};

const bucketKey = {
  app: (scope: string, w: LimitWindow) => `rl:app:${KEY_SCOPE}:${scope}:${w.limit}:${w.seconds}`,
  method: (scope: string, method: string, w: LimitWindow) =>
    `rl:m:${KEY_SCOPE}:${scope}:${method}:${w.limit}:${w.seconds}`,
};

const frozenKey = (scope: string) => `rl:frozen:${KEY_SCOPE}:${scope}`;
const waitersKey = (scope: string) => `rl:waiters:${KEY_SCOPE}:${scope}`;

/** Limit configs change rarely; a short local TTL keeps acquire() to one round trip. */
const CONFIG_CACHE_MS = 10_000;

interface CachedConfig {
  windows: LimitWindow[];
  at: number;
}

export interface AcquireResult {
  /** Milliseconds spent waiting before dispatch was permitted. */
  waitedMs: number;
}

export class RateLimitBudgetExceeded extends Error {
  readonly retryAfterMs: number;
  constructor(retryAfterMs: number, reason: string) {
    super(`Rate limit wait budget exceeded (${reason}, retry in ${retryAfterMs}ms)`);
    this.name = 'RateLimitBudgetExceeded';
    this.retryAfterMs = retryAfterMs;
  }
}

/**
 * §9 — the limiter. Public surface is deliberately just `acquire()` /
 * `observeHeaders()` / `freeze()` so the Redis+Lua implementation can be
 * swapped (e.g. for `@fightmegg/riot-rate-limiter`) without touching callers.
 */
export class RateLimiter {
  private readonly redis: Redis;
  private readonly localConfig = new Map<string, CachedConfig>();
  private acquireSha?: string;
  private syncSha?: string;

  constructor(redis: Redis = defaultRedis) {
    this.redis = redis;
  }

  private async loadScripts(): Promise<{ acquire: string; sync: string }> {
    if (!this.acquireSha || !this.syncSha) {
      const [acquire, sync] = await Promise.all([
        this.redis.script('LOAD', ACQUIRE_SCRIPT) as Promise<string>,
        this.redis.script('LOAD', SYNC_SCRIPT) as Promise<string>,
      ]);
      this.acquireSha = acquire;
      this.syncSha = sync;
    }
    return { acquire: this.acquireSha, sync: this.syncSha };
  }

  private async evalAcquire(keys: string[], argv: (string | number)[]): Promise<unknown> {
    const { acquire } = await this.loadScripts();
    try {
      return await this.redis.evalsha(acquire, keys.length, ...keys, ...argv);
    } catch (err) {
      // Redis was restarted / flushed: reload and retry once.
      if (err instanceof Error && err.message.includes('NOSCRIPT')) {
        this.acquireSha = undefined;
        const reloaded = await this.loadScripts();
        return this.redis.evalsha(reloaded.acquire, keys.length, ...keys, ...argv);
      }
      throw err;
    }
  }

  private async windowsFor(kind: 'app' | 'method', scope: string, method: string) {
    const key = kind === 'app' ? cfgKey.app(scope) : cfgKey.method(scope, method);
    const cached = this.localConfig.get(key);
    if (cached && Date.now() - cached.at < CONFIG_CACHE_MS) return cached.windows;

    const stored = await this.redis.get(key);
    const windows = stored ? parseLimitHeader(stored) : kind === 'app' ? BOOTSTRAP_APP_LIMITS : []; // Method limits are unknown until Riot tells us; app limits still apply.

    this.localConfig.set(key, { windows, at: Date.now() });
    return windows;
  }

  /**
   * Acquire one token from every applicable bucket, queueing until the wait
   * budget is exhausted. `scope` is the platform or region on the host — Riot
   * counts per routing value, not per logical region.
   */
  async acquire(
    scope: string,
    method: string,
    opts: { priority?: Priority; waitBudgetMs?: number } = {},
  ): Promise<AcquireResult> {
    const priority = opts.priority ?? 'interactive';
    const budgetMs =
      opts.waitBudgetMs ?? (priority === 'bulk' ? 120_000 : config.CLIENT_WAIT_BUDGET_MS);
    const started = Date.now();
    const deadline = started + budgetMs;

    // Interactive requests announce themselves so bulk work can stand aside (§9.3).
    const trackWaiters = priority === 'interactive';
    if (trackWaiters) {
      await this.redis.incr(waitersKey(scope));
      await this.redis.expire(waitersKey(scope), 60);
    }

    try {
      for (;;) {
        const [appWindows, methodWindows] = await Promise.all([
          this.windowsFor('app', scope, method),
          this.windowsFor('method', scope, method),
        ]);

        const buckets: (string | number)[] = [];
        let count = 0;
        for (const w of appWindows) {
          buckets.push(bucketKey.app(scope, w), w.limit, w.seconds);
          count += 1;
        }
        for (const w of methodWindows) {
          buckets.push(bucketKey.method(scope, method, w), w.limit, w.seconds);
          count += 1;
        }

        const waiters = trackWaiters
          ? 0 // we are the interactive traffic; do not block ourselves
          : Number((await this.redis.get(waitersKey(scope))) ?? '0');

        // Identifies this attempt's slot in every bucket, so a refusal part-way
        // through can roll back exactly what it took.
        const token = randomUUID();

        const raw = (await this.evalAcquire(
          [frozenKey(scope)],
          [priority, config.BULK_USAGE_CEILING, waiters, token, count, ...buckets],
        )) as [number, number, string];

        const [ok, waitMs, reason] = raw;
        if (ok === 1) {
          const waitedMs = Date.now() - started;
          rlWaitSeconds.observe({ region: scope, priority }, waitedMs / 1000);
          return { waitedMs };
        }

        const now = Date.now();
        if (now >= deadline) {
          rlWaitSeconds.observe({ region: scope, priority }, (now - started) / 1000);
          throw new RateLimitBudgetExceeded(waitMs, reason);
        }

        // Sleep the shorter of "until the bucket frees" and "until our budget
        // runs out", with jitter so N instances do not wake in lockstep.
        const jitter = Math.floor(Math.random() * 50);
        const sleepMs = Math.max(10, Math.min(waitMs + jitter, deadline - now, 1000));
        await delay(sleepMs);
      }
    } finally {
      if (trackWaiters) {
        await this.redis.decr(waitersKey(scope)).catch(() => undefined);
      }
    }
  }

  /**
   * §9.1 — re-configure buckets from response headers on every response, and
   * sync counts to absorb drift from other users of the same key.
   */
  async observeHeaders(
    scope: string,
    method: string,
    headers: Record<string, string | undefined>,
  ): Promise<void> {
    const tasks: Promise<unknown>[] = [];

    const appLimit = headers['x-app-rate-limit'];
    if (appLimit) {
      tasks.push(this.storeConfig(cfgKey.app(scope), appLimit));
    }
    const methodLimit = headers['x-method-rate-limit'];
    if (methodLimit) {
      tasks.push(this.storeConfig(cfgKey.method(scope, method), methodLimit));
    }

    const syncArgs: (string | number)[] = [];
    let count = 0;
    count += this.collectSync(
      syncArgs,
      parseLimitHeader(appLimit),
      parseLimitHeader(headers['x-app-rate-limit-count']),
      (w) => bucketKey.app(scope, w),
    );
    count += this.collectSync(
      syncArgs,
      parseLimitHeader(methodLimit),
      parseLimitHeader(headers['x-method-rate-limit-count']),
      (w) => bucketKey.method(scope, method, w),
    );

    if (count > 0) tasks.push(this.evalSync([count, ...syncArgs]));

    await Promise.allSettled(tasks);
  }

  /**
   * The `-Count` header mirrors the limit header's window order, but reports
   * `used:window` instead of `limit:window`. Match them up by window length
   * rather than position so a reordered header cannot corrupt accounting.
   */
  private collectSync(
    out: (string | number)[],
    limits: LimitWindow[],
    counts: LimitWindow[],
    keyFor: (w: LimitWindow) => string,
  ): number {
    let added = 0;
    for (const limitWindow of limits) {
      const used = counts.find((c) => c.seconds === limitWindow.seconds);
      if (!used) continue;
      out.push(keyFor(limitWindow), used.limit, limitWindow.seconds);
      added += 1;
    }
    return added;
  }

  private async storeConfig(key: string, header: string): Promise<void> {
    const cached = this.localConfig.get(key);
    const parsed = parseLimitHeader(header);
    // Skip the write when nothing changed — this runs on every single response.
    if (cached && serialiseWindows(cached.windows) === serialiseWindows(parsed)) return;
    this.localConfig.set(key, { windows: parsed, at: Date.now() });
    await this.redis.set(key, header, 'EX', 86_400);
  }

  private async evalSync(argv: (string | number)[]): Promise<void> {
    const { sync } = await this.loadScripts();
    try {
      await this.redis.evalsha(sync, 0, ...argv);
    } catch (err) {
      if (err instanceof Error && err.message.includes('NOSCRIPT')) {
        this.syncSha = undefined;
        const reloaded = await this.loadScripts();
        await this.redis.evalsha(reloaded.sync, 0, ...argv);
      } else {
        throw err;
      }
    }
  }

  /**
   * §9.4 — a 429 carrying `Retry-After` means Riot's edge is enforcing; freeze
   * the whole scope until the deadline rather than letting instances trickle
   * more requests into a closed door.
   */
  async freeze(scope: string, seconds: number, type = 'unknown'): Promise<void> {
    const ms = Math.max(1, Math.ceil(seconds * 1000));
    await this.redis.set(frozenKey(scope), type, 'PX', ms);
    // `proxy_rl_429_total` is owned by the client, which observes every 429.
    // Counting here as well double-counted exactly the accountable types
    // (§9.4 freezes only those), inflating the series §13 alerts on.
    logger.warn({ scope, seconds, type }, 'rate limit scope frozen');
  }

  async isFrozen(scope: string): Promise<number> {
    const pttl = await this.redis.pttl(frozenKey(scope));
    return pttl > 0 ? pttl : 0;
  }

  /**
   * Current usage per app window — used by `/healthz` and the admin surface.
   * Counts what is still inside each window without trimming, so a read can
   * never alter what the next acquisition sees.
   */
  async usage(scope: string): Promise<{ window: string; used: number; limit: number }[]> {
    const windows = await this.windowsFor('app', scope, '');
    if (windows.length === 0) return [];

    const now = Date.now();
    const pipeline = this.redis.pipeline();
    for (const w of windows) {
      pipeline.zcount(bucketKey.app(scope, w), `(${now - w.seconds * 1000}`, '+inf');
    }
    const results = await pipeline.exec();

    return windows.map((w, i) => ({
      window: `${w.limit}:${w.seconds}`,
      used: Number(results?.[i]?.[1] ?? 0),
      limit: w.limit,
    }));
  }

  /** Test helper: forget locally cached limit configs. */
  clearLocalConfig(): void {
    this.localConfig.clear();
  }
}

function serialiseWindows(windows: LimitWindow[]): string {
  return windows
    .map((w) => `${w.limit}:${w.seconds}`)
    .sort()
    .join(',');
}

export function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const limiter = new RateLimiter();

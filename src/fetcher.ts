import { cacheKey, negativeKey } from './cache/keys.js';
import { cacheStore, type CacheState, type CacheStore } from './cache/store.js';
import { singleFlight, type SingleFlight } from './cache/singleflight.js';
import { config } from './config.js';
import {
  archiveMatch,
  archiveTimeline,
  getArchivedMatch,
  getArchivedTimeline,
  type RiotMatch,
} from './db/matches.js';
import { ProxyError, RiotError } from './errors.js';
import { logger } from './logger.js';
import { recordCacheOutcome } from './metrics.js';
import { riotClient, type RiotClient } from './riot/client.js';
import type { BuiltRequest } from './riot/endpoints.js';
import { RateLimitBudgetExceeded, type Priority } from './riot/limiter.js';
import { regionFromMatchId } from './riot/routing.js';

export interface FetchResult<T> {
  data: T;
  cache: CacheState;
  ageSeconds: number;
}

export interface FetchOptions {
  priority?: Priority;
  waitBudgetMs?: number;
  /** Skip the cache read (still writes). Used by the SWR refresher and admin. */
  bypassCache?: boolean;
  signal?: AbortSignal;
}

/**
 * §3.2 — the read path, in one place:
 *
 *   archive (immutable only) → negative cache → cache → single-flight
 *   → limiter → upstream → cache write → archive write
 *
 * Every public route funnels through here so the caching, coalescing and
 * limiting guarantees hold uniformly.
 */
export class Fetcher {
  constructor(
    private readonly client: RiotClient = riotClient,
    private readonly store: CacheStore = cacheStore,
    private readonly flight: SingleFlight = singleFlight,
  ) {}

  async fetch<T>(req: BuiltRequest, opts: FetchOptions = {}): Promise<FetchResult<T>> {
    const key = cacheKey(req);
    const negKey = negativeKey(req);

    if (!opts.bypassCache) {
      // 1. Postgres archive short-circuits immutable data entirely (§7.3).
      const archived = await this.readArchive<T>(req);
      if (archived !== undefined) {
        recordCacheOutcome('hit');
        return { data: archived, cache: 'HIT', ageSeconds: 0 };
      }

      // 2. Negative cache (§8.3) — cheaper than a miss and distinguishable.
      if (req.spec.negTtlSeconds > 0 && (await this.store.getNegative(negKey))) {
        recordCacheOutcome('neg');
        throw ProxyError.notFound('Resource not found upstream (negative-cached)');
      }

      // 3. Positive cache, honouring stale-while-revalidate (§8.5).
      const hit = await this.store.get<T>(key);
      if (hit && !hit.stale) {
        recordCacheOutcome('hit');
        return { data: hit.value, cache: 'HIT', ageSeconds: hit.ageSeconds };
      }
      if (hit && hit.stale) {
        recordCacheOutcome('stale');
        this.refreshInBackground(req, key, negKey);
        return { data: hit.value, cache: 'STALE', ageSeconds: hit.ageSeconds };
      }
    }

    // 4. Miss — coalesce, then go upstream (§8.4).
    recordCacheOutcome('miss');
    const result = await this.flight.run<T>(key, {
      work: () => this.upstream<T>(req, key, negKey, opts),
      peek: async () => {
        const entry = await this.store.get<T>(key);
        return entry?.value;
      },
    });

    return { data: result.value, cache: 'MISS', ageSeconds: 0 };
  }

  /** The upstream leg: limiter + HTTP + cache/archive writes and 404 negatives. */
  private async upstream<T>(
    req: BuiltRequest,
    key: string,
    negKey: string,
    opts: FetchOptions,
  ): Promise<T> {
    try {
      const res = await this.client.request<T>(req, {
        priority: opts.priority ?? 'interactive',
        ...(opts.waitBudgetMs !== undefined ? { waitBudgetMs: opts.waitBudgetMs } : {}),
        ...(opts.signal ? { signal: opts.signal } : {}),
      });

      await this.store.set(key, res.data, req.spec.ttlSeconds);
      this.writeArchive(req, res.data);
      return res.data;
    } catch (err) {
      if (err instanceof RiotError) {
        if (err.isNotFound && req.spec.negTtlSeconds > 0) {
          await this.store.setNegative(negKey, req.spec.negTtlSeconds);
        }
        // §8.5 — serve stale rather than an error when upstream is broken.
        if (err.isServerError || err.status === 0) {
          const stale = await this.store.get<T>(key);
          if (stale) {
            logger.warn(
              { method: req.method, status: err.status },
              'serving stale on upstream 5xx',
            );
            return stale.value;
          }
        }
        throw err.toProxyError();
      }

      if (err instanceof RateLimitBudgetExceeded) {
        // §5.5 — the wait exceeds the client's budget; hand back a retry hint
        // instead of holding the connection open.
        const stale = await this.store.get<T>(key);
        if (stale) {
          logger.warn({ method: req.method }, 'serving stale while rate limited');
          return stale.value;
        }
        throw ProxyError.rateLimited(Math.ceil(err.retryAfterMs / 1000));
      }

      throw err;
    }
  }

  /** §8.5 — refresh past the soft TTL without making the caller wait. */
  private refreshInBackground(req: BuiltRequest, key: string, negKey: string): void {
    if (!config.STALE_WHILE_REVALIDATE) return;
    void this.flight
      .run(key, {
        work: () => this.upstream(req, key, negKey, { priority: 'bulk' }),
        peek: async () => (await this.store.get(key))?.value,
      })
      .catch((err: unknown) => {
        logger.debug({ err, method: req.method }, 'background refresh failed');
      });
  }

  private async readArchive<T>(req: BuiltRequest): Promise<T | undefined> {
    if (!req.spec.immutable) return undefined;
    const matchId = matchIdFrom(req);
    if (!matchId) return undefined;
    try {
      const row =
        req.method === 'match.timeline'
          ? await getArchivedTimeline(matchId)
          : await getArchivedMatch(matchId);
      return (row as T | undefined) ?? undefined;
    } catch (err) {
      // A degraded archive must not break reads — fall through to Redis/Riot.
      logger.warn({ err, matchId }, 'archive read failed');
      return undefined;
    }
  }

  /**
   * Archive-on-fetch (Phase 5). Written directly rather than via the queue so
   * the archive still fills when the worker is down; `archive:match` covers the
   * worker-initiated path.
   */
  private writeArchive(req: BuiltRequest, data: unknown): void {
    if (!req.spec.immutable) return;
    const matchId = matchIdFrom(req);
    if (!matchId) return;
    const region = regionFromMatchId(matchId) ?? req.scope;

    const task =
      req.method === 'match.timeline'
        ? archiveTimeline(matchId, region, data)
        : archiveMatch(matchId, region, data as RiotMatch);

    void task.catch((err: unknown) => {
      logger.warn({ err, matchId, method: req.method }, 'archive write failed');
    });
  }
}

/** Recover the match id from the built path — the archive keys on it. */
function matchIdFrom(req: BuiltRequest): string | undefined {
  const parts = req.path.split('/');
  const idx = parts.indexOf('matches');
  if (idx === -1) return undefined;
  const candidate = parts[idx + 1];
  if (!candidate) return undefined;
  return decodeURIComponent(candidate);
}

export const fetcher = new Fetcher();

import type { FastifyReply } from 'fastify';
import type { CacheState } from '../cache/store.js';
import type { FetchResult } from '../fetcher.js';

/** §6.1 — proxy headers accompany every cached read. */
export function applyCacheHeaders(reply: FastifyReply, state: CacheState, ageSeconds: number) {
  reply.header('X-Cache', state);
  reply.header('X-Cache-Age', String(ageSeconds));
}

export function send<T>(reply: FastifyReply, result: FetchResult<T>): T {
  applyCacheHeaders(reply, result.cache, result.ageSeconds);
  return result.data;
}

/** Route-level tag used by the request metric label, kept short and stable. */
export function routeLabel(url: string | undefined): string {
  return url ?? 'unknown';
}

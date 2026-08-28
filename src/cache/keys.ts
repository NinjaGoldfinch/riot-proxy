import { createHash } from 'node:crypto';
import { KEY_SCOPE } from '../config.js';
import type { BuiltRequest } from '../riot/endpoints.js';

/**
 * §8.1 — canonical cache key:
 *   c:{key_scope}:{riot-method-id}:{host}:{sha1(sorted path+query)}
 *
 * Query params are sorted before hashing so `?start=0&count=20` and
 * `?count=20&start=0` collide correctly.
 */
export function canonicalTarget(path: string, query: Record<string, unknown> = {}): string {
  const entries = Object.entries(query)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => [k, String(v)] as const)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));

  if (entries.length === 0) return path;
  const qs = entries.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join('&');
  return `${path}?${qs}`;
}

export function hashTarget(target: string): string {
  return createHash('sha1').update(target).digest('hex');
}

export function cacheKey(req: BuiltRequest): string {
  return `c:${KEY_SCOPE}:${req.method}:${req.host}:${hashTarget(canonicalTarget(req.path, req.query))}`;
}

/** §8.3 — negative markers live under a distinct prefix so a cached 404 is
 * distinguishable from "unknown". */
export function negativeKey(req: BuiltRequest): string {
  return `neg:${KEY_SCOPE}:${req.method}:${req.host}:${hashTarget(canonicalTarget(req.path, req.query))}`;
}

/** §8.4 — single-flight lock derived from the cache key. */
export function singleFlightKey(key: string): string {
  return `sf:${key}`;
}

/**
 * Admin cache purge (§6.2) works on glob patterns. Scope every pattern to the
 * current key scope so a purge cannot wipe another deployment's namespace.
 */
export function scopedPurgePattern(pattern: string): string {
  if (pattern.startsWith('c:') || pattern.startsWith('neg:')) return pattern;
  return `c:${KEY_SCOPE}:${pattern}`;
}

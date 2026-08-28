import { isIP } from 'node:net';
import type { FastifyPluginAsync, FastifyRequest } from 'fastify';
import fp from 'fastify-plugin';
import { config } from '../config.js';
import { findConsumerByHash } from '../db/consumers.js';
import { hashKey } from '../db/consumers.js';
import { ProxyError } from '../errors.js';
import { logger } from '../logger.js';
import { redis } from '../redis.js';

export interface AuthenticatedConsumer {
  id: string;
  name: string;
  scopes: string[];
  quotaPerMin: number;
}

declare module 'fastify' {
  interface FastifyRequest {
    consumer?: AuthenticatedConsumer;
  }
  interface FastifyContextConfig {
    /** Routes are authenticated unless they opt out (health, metrics). */
    public?: boolean;
    /** Scope required beyond `read`. */
    scope?: 'read' | 'admin';
  }
}

/** §7.2 — `auth:{key_hash}` caches the consumer row for 300 s. */
const AUTH_CACHE_TTL = 300;
const NEGATIVE_AUTH_TTL = 30;

export function bearerFrom(request: FastifyRequest): string | undefined {
  const header = request.headers.authorization;
  if (header) {
    const [scheme, value] = header.split(' ');
    if (scheme?.toLowerCase() === 'bearer' && value) return value.trim();
  }
  // The WS handshake cannot set headers in a browser, so `?token=` is allowed
  // there (§11). Accepted on any route for symmetry, never logged.
  const query = request.query as Record<string, unknown> | undefined;
  const token = query?.['token'];
  return typeof token === 'string' && token ? token : undefined;
}

/**
 * Resolve a bearer token to a consumer, with a Redis read-through cache.
 * Returns undefined for unknown/disabled keys (also cached, briefly, so a
 * key-guessing flood cannot hammer Postgres).
 */
export async function resolveConsumer(key: string): Promise<AuthenticatedConsumer | undefined> {
  const keyHash = hashKey(key);
  const cacheKey = `auth:${keyHash}`;

  const cached = await redis.get(cacheKey);
  if (cached === '0') return undefined;
  if (cached) {
    try {
      return JSON.parse(cached) as AuthenticatedConsumer;
    } catch {
      await redis.del(cacheKey).catch(() => undefined);
    }
  }

  const row = await findConsumerByHash(keyHash);
  if (!row) {
    await redis.set(cacheKey, '0', 'EX', NEGATIVE_AUTH_TTL);
    return undefined;
  }

  const consumer: AuthenticatedConsumer = {
    id: row.id,
    name: row.name,
    scopes: row.scopes,
    quotaPerMin: row.quotaPerMin,
  };
  await redis.set(cacheKey, JSON.stringify(consumer), 'EX', AUTH_CACHE_TTL);
  return consumer;
}

/** Invalidate the auth cache when a key is revoked, so revocation is immediate. */
export async function invalidateAuthCache(keyHash: string): Promise<void> {
  await redis.del(`auth:${keyHash}`).catch(() => undefined);
}

/**
 * §12.1 — admin routes are additionally IP-allowlisted. Exact IPs and CIDR
 * blocks are supported; an empty allowlist means "key scope is enough".
 */
export function ipAllowed(remote: string | undefined, allowlist: string[]): boolean {
  if (allowlist.length === 0) return true;
  if (!remote) return false;
  const addr = normaliseAddress(remote);
  for (const entry of allowlist) {
    if (entry.includes('/')) {
      if (cidrMatch(addr, entry)) return true;
    } else if (normaliseAddress(entry) === addr) {
      return true;
    }
  }
  return false;
}

/** `::ffff:127.0.0.1` and `127.0.0.1` are the same host. */
function normaliseAddress(addr: string): string {
  const trimmed = addr.trim();
  return trimmed.startsWith('::ffff:') ? trimmed.slice(7) : trimmed;
}

function cidrMatch(addr: string, cidr: string): boolean {
  const [base, bitsRaw] = cidr.split('/');
  if (!base || bitsRaw === undefined) return false;
  const bits = Number(bitsRaw);
  if (!Number.isInteger(bits) || bits < 0) return false;
  // IPv4 only; IPv6 ranges should be listed explicitly.
  if (isIP(base) !== 4 || isIP(addr) !== 4 || bits > 32) return false;

  const toInt = (ip: string) =>
    ip.split('.').reduce((acc, octet) => (acc << 8) + Number(octet), 0) >>> 0;
  const mask = bits === 0 ? 0 : (0xffffffff << (32 - bits)) >>> 0;
  return (toInt(addr) & mask) === (toInt(base) & mask);
}

const authPlugin: FastifyPluginAsync = async (fastify) => {
  fastify.addHook('onRequest', async (request: FastifyRequest) => {
    if (request.routeOptions.config?.public) return;

    const token = bearerFrom(request);
    if (!token) throw ProxyError.unauthorized();

    const consumer = await resolveConsumer(token);
    if (!consumer) {
      logger.warn({ ip: request.ip, route: request.routeOptions.url }, 'rejected unknown key');
      throw ProxyError.unauthorized();
    }

    const required = request.routeOptions.config?.scope ?? 'read';
    if (!consumer.scopes.includes(required)) {
      throw new ProxyError('FORBIDDEN', `This key lacks the '${required}' scope`);
    }

    if (required === 'admin' && !ipAllowed(request.ip, config.adminIpAllowlist)) {
      logger.warn({ ip: request.ip, consumer: consumer.name }, 'admin request from disallowed IP');
      throw new ProxyError('FORBIDDEN', 'Admin access is not permitted from this address');
    }

    request.consumer = consumer;
  });
};

export default fp(authPlugin, { name: 'auth' });

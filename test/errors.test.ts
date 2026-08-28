import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { ProxyError, RiotError } from '../src/errors.js';

describe('error envelope (§6.1)', () => {
  it('maps each code to its documented status', () => {
    expect(new ProxyError('UNAUTHORIZED', 'x').statusCode).toBe(401);
    expect(new ProxyError('QUOTA_EXCEEDED', 'x').statusCode).toBe(429);
    expect(new ProxyError('NOT_FOUND', 'x').statusCode).toBe(404);
    expect(new ProxyError('UPSTREAM_ERROR', 'x').statusCode).toBe(502);
    expect(new ProxyError('RATE_LIMITED', 'x').statusCode).toBe(503);
    expect(new ProxyError('BAD_REGION', 'x').statusCode).toBe(400);
    expect(new ProxyError('VALIDATION', 'x').statusCode).toBe(400);
  });

  it('serialises to the single envelope shape, omitting retryAfter when absent', () => {
    expect(new ProxyError('NOT_FOUND', 'gone').toEnvelope()).toEqual({
      error: { code: 'NOT_FOUND', message: 'gone' },
    });
    expect(ProxyError.rateLimited(3).toEnvelope()).toEqual({
      error: {
        code: 'RATE_LIMITED',
        message: 'Upstream rate limit budget exceeded',
        retryAfter: 3,
      },
    });
  });
});

describe('upstream error policy (§5.5)', () => {
  const at = (status: number, extra = {}) =>
    new RiotError(status, 'summoner.byPuuid', 'euw1.api.riotgames.com', 'x', extra);

  it('turns an upstream 404 into a client 404', () => {
    expect(at(404).toProxyError().code).toBe('NOT_FOUND');
  });

  it('never tells the client the Riot key was rejected', () => {
    for (const status of [401, 403]) {
      const proxied = at(status).toProxyError();
      expect(proxied.code).toBe('UPSTREAM_ERROR');
      expect(proxied.statusCode).toBe(502);
      expect(proxied.message).not.toMatch(/key/i);
    }
  });

  it('carries Retry-After through a 429', () => {
    const proxied = at(429, { retryAfter: 7 }).toProxyError();
    expect(proxied.code).toBe('RATE_LIMITED');
    expect(proxied.retryAfter).toBe(7);
  });

  it('maps 5xx to UPSTREAM_ERROR and classifies statuses', () => {
    expect(at(503).toProxyError().code).toBe('UPSTREAM_ERROR');
    expect(at(503).isServerError).toBe(true);
    expect(at(404).isNotFound).toBe(true);
    expect(at(403).isAuthFailure).toBe(true);
    expect(at(429).isRateLimited).toBe(true);
  });
});

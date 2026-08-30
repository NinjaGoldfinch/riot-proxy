import './helpers/env.js';
import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { KEY_SCOPE, __test, parseTtlOverrides } from '../src/config.js';
import { scrubKey } from '../src/logger.js';

const { parseEnv } = __test;

describe('config (§14, §7.4)', () => {
  it('fails fast when RIOT_API_KEY is missing (Phase 0)', () => {
    expect(() => parseEnv({})).toThrowError(/RIOT_API_KEY|Invalid environment/i);
  });

  it('applies documented defaults', () => {
    const env = parseEnv({ RIOT_API_KEY: 'RGAPI-abc-def-ghi' });
    expect(env.PORT).toBe(8080);
    expect(env.NEG_TTL_SECONDS).toBe(30);
    expect(env.SF_LOCK_MS).toBe(5000);
    expect(env.CLIENT_WAIT_BUDGET_MS).toBe(2000);
    expect(env.BULK_USAGE_CEILING).toBe(0.8);
    expect(env.TRACK_POLL_LIVE_S).toBe(60);
    expect(env.TRACK_POLL_RANK_S).toBe(600);
    expect(env.TRACK_POLL_MATCH_S).toBe(300);
  });

  it('coerces numeric and boolean strings', () => {
    const env = parseEnv({
      RIOT_API_KEY: 'RGAPI-abc-def-ghi',
      PORT: '9090',
      STALE_WHILE_REVALIDATE: 'false',
      BULK_USAGE_CEILING: '0.5',
    });
    expect(env.PORT).toBe(9090);
    expect(env.STALE_WHILE_REVALIDATE).toBe(false);
    expect(env.BULK_USAGE_CEILING).toBe(0.5);
  });

  it('treats an empty value as absent so defaults still apply', () => {
    expect(parseEnv({ RIOT_API_KEY: 'RGAPI-abc-def-ghi', PORT: '' }).PORT).toBe(8080);
  });

  it('defaults AUTH_DISABLED off and coerces it when set', () => {
    const key = 'RGAPI-abc-def-ghi';
    expect(parseEnv({ RIOT_API_KEY: key }).AUTH_DISABLED).toBe(false);
    expect(parseEnv({ RIOT_API_KEY: key, AUTH_DISABLED: 'true' }).AUTH_DISABLED).toBe(true);
  });

  it('refuses to boot with AUTH_DISABLED in production', () => {
    expect(() =>
      parseEnv({
        RIOT_API_KEY: 'RGAPI-abc-def-ghi',
        AUTH_DISABLED: 'true',
        NODE_ENV: 'production',
      }),
    ).toThrowError(/AUTH_DISABLED/);
  });

  it('rejects out-of-range values', () => {
    expect(() =>
      parseEnv({ RIOT_API_KEY: 'RGAPI-abc-def-ghi', BULK_USAGE_CEILING: '2' }),
    ).toThrowError();
    expect(() => parseEnv({ RIOT_API_KEY: 'RGAPI-abc-def-ghi', PORT: '0' })).toThrowError();
  });

  it('derives KEY_SCOPE as the first 8 hex chars of sha256(key) (§7.4)', () => {
    const expected = createHash('sha256')
      .update(process.env['RIOT_API_KEY'] as string)
      .digest('hex')
      .slice(0, 8);
    expect(KEY_SCOPE).toBe(expected);
    expect(KEY_SCOPE).toHaveLength(8);
  });

  it('parses the CACHE_TTL_OVERRIDES CSV format', () => {
    expect(parseTtlOverrides('league=120,spectator=20')).toEqual({ league: 120, spectator: 20 });
    expect(parseTtlOverrides(' league = 120 ')).toEqual({ league: 120 });
    expect(parseTtlOverrides('')).toEqual({});
    expect(parseTtlOverrides('garbage,league=abc,=5,x=-1')).toEqual({});
  });
});

describe('log redaction (§12.2)', () => {
  it('scrubs the configured Riot key out of any string', () => {
    const key = process.env['RIOT_API_KEY'] as string;
    expect(scrubKey(`X-Riot-Token: ${key}`)).not.toContain(key);
    expect(scrubKey(`X-Riot-Token: ${key}`)).toContain('[REDACTED]');
  });

  it('scrubs any RGAPI token, not just ours', () => {
    const other = 'RGAPI-11111111-2222-3333-4444-555555555555';
    expect(scrubKey(`key=${other}`)).not.toContain(other);
  });

  it('scrubs proxy-issued consumer keys too', () => {
    const consumerKey = 'rpx_abcdefghijklmnopqrstuvwxyz012345';
    expect(scrubKey(`Bearer ${consumerKey}`)).not.toContain(consumerKey);
  });

  it('leaves ordinary strings untouched', () => {
    expect(scrubKey('nothing secret here')).toBe('nothing secret here');
  });
});

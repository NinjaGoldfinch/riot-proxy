import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { BOOTSTRAP_APP_LIMITS, parseLimitHeader } from '../src/riot/limiter.js';

describe('limiter header parsing (§5.4, §9.1)', () => {
  it('parses the documented header format', () => {
    expect(parseLimitHeader('20:1,100:120')).toEqual([
      { limit: 20, seconds: 1 },
      { limit: 100, seconds: 120 },
    ]);
  });

  it('tolerates whitespace and a single window', () => {
    expect(parseLimitHeader(' 500:10 ')).toEqual([{ limit: 500, seconds: 10 }]);
  });

  it('returns an empty list for missing or malformed headers', () => {
    expect(parseLimitHeader(undefined)).toEqual([]);
    expect(parseLimitHeader('')).toEqual([]);
    expect(parseLimitHeader('garbage')).toEqual([]);
    expect(parseLimitHeader('0:1')).toEqual([]);
    expect(parseLimitHeader('20:0')).toEqual([]);
  });

  it('skips malformed windows but keeps valid ones', () => {
    expect(parseLimitHeader('20:1,bad,100:120')).toEqual([
      { limit: 20, seconds: 1 },
      { limit: 100, seconds: 120 },
    ]);
  });

  it('bootstraps with the documented development-key limits (§2.3)', () => {
    expect(BOOTSTRAP_APP_LIMITS).toEqual([
      { limit: 20, seconds: 1 },
      { limit: 100, seconds: 120 },
    ]);
  });
});

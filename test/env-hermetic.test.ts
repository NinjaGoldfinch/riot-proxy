import { CONNECTIONS, PINNED } from './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { __test, config } from '../src/config.js';

/**
 * The helper is only hermetic while it covers every setting the app reads. A
 * new key in `EnvSchema` that nobody pins here is one a future test can
 * silently inherit from a developer's `.env`, and the resulting failure points
 * at the code rather than at the machine — so assert the coverage instead of
 * relying on the comment asking for it.
 */
describe('test environment (§4.2)', () => {
  it('pins or explicitly excepts every setting the app reads', () => {
    const declared = Object.keys(__test.EnvSchema.properties).sort();
    const covered = [...Object.keys(CONNECTIONS), ...Object.keys(PINNED)].sort();
    expect(declared.filter((key) => !covered.includes(key))).toEqual([]);
  });

  it('leaves connection strings machine-specific and everything else fixed', () => {
    // The two that must be allowed to vary, and nothing else.
    expect(Object.keys(CONNECTIONS).sort()).toEqual(['DATABASE_URL', 'REDIS_URL']);
  });

  it('holds the settings the suite actually asserts on, whatever the env says', () => {
    // Each of these has burned us or could: STALE_WHILE_REVALIDATE changes a
    // TTL assertion, and AUTH_DISABLED would make the auth tests pass without
    // asserting anything — the dangerous direction.
    expect(config.STALE_WHILE_REVALIDATE).toBe(true);
    expect(config.AUTH_DISABLED).toBe(false);
    expect(config.NODE_ENV).toBe('test');
    expect(config.BULK_USAGE_CEILING).toBe(0.8);
    expect(config.DEFAULT_PLATFORM).toBe('euw1');
  });
});

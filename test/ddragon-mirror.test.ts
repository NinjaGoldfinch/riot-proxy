import './helpers/env.js';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, describe, expect, it, vi } from 'vitest';

/**
 * The Data Dragon mirror on disk (§5.6, FR-11).
 *
 * `readStatic` takes a version that reaches it from a query string, and joins
 * it onto `DDRAGON_DIR`. `join` normalises `..`, so an unchecked segment reads
 * whatever the path walks out to — and the reply hands the body straight back
 * to the caller. The route's schema is the first guard; this file covers the
 * second one, inside the exported function, because the route is not the only
 * possible caller.
 *
 * Hermetic by construction: the mirror is a temp directory built here, and
 * Redis is faked, so neither the developer's `data/ddragon` nor a running
 * service changes what these assert.
 */
vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

// A parent holding both the mirror and a file outside it, so "outside" is a
// real readable file rather than a path that happens not to exist.
const base = await mkdtemp(join(tmpdir(), 'ddragon-test-'));
const mirror = join(base, 'ddragon');
const VERSION = '16.17.1';

await mkdir(join(mirror, VERSION), { recursive: true });
await writeFile(join(mirror, VERSION, 'champion.json'), JSON.stringify({ type: 'champion' }));
await writeFile(join(base, 'secret.json'), JSON.stringify({ probe: 'leaked' }));

// Set before config.ts is first imported: it reads env once, at import time.
process.env.DDRAGON_DIR = mirror;

const { DATA_FILES, ddragonDir, readStatic } = await import('../src/static/ddragon.js');

afterAll(() => vi.restoreAllMocks());

describe('data dragon mirror (§5.6)', () => {
  it('reads a mirrored file for an explicit version', async () => {
    expect(ddragonDir()).toBe(mirror);
    expect(await readStatic('champion', VERSION)).toEqual({ type: 'champion' });
  });

  it('returns undefined for a version that was never synced', async () => {
    expect(await readStatic('champion', '1.2.3')).toBeUndefined();
  });

  it('refuses a version segment that walks out of DDRAGON_DIR (#51)', async () => {
    expect(await readStatic('secret', '..')).toBeUndefined();
    expect(await readStatic('secret', `${VERSION}/..`)).toBeUndefined();
    expect(await readStatic('champion', '../../../../../../etc')).toBeUndefined();
  });

  it('refuses a file segment that walks out too, since both are joined', async () => {
    // The route constrains `file` to an enum, but the check covers the whole
    // resolved path rather than trusting one caller to have done that.
    expect(await readStatic('../../secret', VERSION)).toBeUndefined();
  });

  /**
   * Data Dragon does not serve `queue.json` — it 403s. Queue metadata lives at
   * static.developer.riotgames.com, outside Data Dragon entirely, so mirroring
   * it here only ever cost a request and a warning per sync (#52).
   */
  it('does not claim queue.json is a Data Dragon file (#52)', () => {
    expect(DATA_FILES).not.toContain('queue');
  });
});

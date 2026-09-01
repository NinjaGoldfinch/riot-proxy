import './helpers/env.js';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it, vi } from 'vitest';

/**
 * Champion id → name from the Data Dragon mirror (#111). Hermetic like
 * `ddragon-mirror.test.ts`: a temp mirror on disk and a faked Redis, so
 * neither a developer's `data/ddragon` nor a running service's
 * `ddragon:version` changes what these assert.
 */
vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

const base = await mkdtemp(join(tmpdir(), 'champion-names-test-'));
const mirror = join(base, 'ddragon');
const V1 = '16.17.1';
const V2 = '16.18.1';

async function writeChampionFile(
  version: string,
  data: Record<string, { key: string; name: string }>,
) {
  await mkdir(join(mirror, version), { recursive: true });
  await writeFile(
    join(mirror, version, 'champion.json'),
    JSON.stringify({ type: 'champion', data }),
  );
}

await writeChampionFile(V1, {
  Ahri: { key: '103', name: 'Ahri' },
  Garen: { key: '86', name: 'Garen' },
});
await writeChampionFile(V2, {
  // A stand-in for "the mirror moved" — same id, a name a re-parse would have
  // to actually happen to see.
  Ahri: { key: '103', name: 'Ahri (v2 fixture)' },
});

// Set before config.ts is first imported: it reads env once, at import time.
process.env.DDRAGON_DIR = mirror;

const { redis } = await import('../src/redis.js');
const { championNames } = await import('../src/static/champions.js');

const VERSION_KEY = 'ddragon:version';

describe('champion names from the Data Dragon mirror', () => {
  it('resolves a batch of ids to names', async () => {
    await redis.set(VERSION_KEY, V1);
    const names = await championNames([103, 86]);
    expect(names.get(103)).toBe('Ahri');
    expect(names.get(86)).toBe('Garen');
  });

  it('omits ids the mirror does not know, rather than guessing', async () => {
    await redis.set(VERSION_KEY, V1);
    const names = await championNames([103, 999_999]);
    expect(names.has(999_999)).toBe(false);
    expect(names.size).toBe(1);
  });

  it('re-reads once the mirrored version moves', async () => {
    await redis.set(VERSION_KEY, V1);
    expect((await championNames([103])).get(103)).toBe('Ahri');

    await redis.set(VERSION_KEY, V2);
    expect((await championNames([103])).get(103)).toBe('Ahri (v2 fixture)');

    // And back — proves this is a real per-version cache, not a one-shot
    // "parsed once, never again".
    await redis.set(VERSION_KEY, V1);
    expect((await championNames([103])).get(103)).toBe('Ahri');
  });
});

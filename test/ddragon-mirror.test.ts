import './helpers/env.js';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeEach, describe, expect, it, vi } from 'vitest';

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

/**
 * Data Dragon over a stubbed undici: a URL either has a body registered or it
 * 403s, which is exactly how Riot answers a file it does not publish for a
 * patch — the case `syncDdragon` has to survive rather than abort on.
 */
const cdn = vi.hoisted(() => ({ routes: new Map<string, unknown>(), requested: [] as string[] }));

vi.mock('undici', () => ({
  request: async (url: string) => {
    cdn.requested.push(url);
    const body = cdn.routes.get(url);
    if (body === undefined) {
      return { statusCode: 403, body: { dump: async () => {}, json: async () => ({}) } };
    }
    return { statusCode: 200, body: { dump: async () => {}, json: async () => body } };
  },
}));

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

const {
  DATA_FILES,
  DDRAGON_BASE,
  VERSIONS_URL,
  compareVersions,
  currentVersion,
  ddragonDir,
  readStatic,
  syncDdragon,
} = await import('../src/static/ddragon.js');
const { FILE_ALIASES } = await import('../src/routes/static.js');
const { config } = await import('../src/config.js');
const { redis } = await import('../src/redis.js');

const VERSION_KEY = 'ddragon:version';
const dataUrl = (version: string, file: string) =>
  `${DDRAGON_BASE}/cdn/${version}/data/${config.DDRAGON_LOCALE}/${file}.json`;

/** Everything Riot serves for one patch, so a sync of it succeeds outright. */
function publishPatch(version: string, files: readonly string[] = DATA_FILES): void {
  cdn.routes.set(VERSIONS_URL, [version, VERSION]);
  for (const file of files) cdn.routes.set(dataUrl(version, file), { file, version });
}

beforeEach(() => {
  cdn.routes.clear();
  cdn.requested.length = 0;
});

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

/**
 * The version comparison is hand-rolled, and it exists because the obvious
 * thing — string ordering — is wrong for exactly the patches that matter:
 * '9.24.1' sorts above '16.17.1' lexically, so a mirror would keep serving last
 * year's champions after every major bump.
 */
describe('patch ordering', () => {
  it('compares patches numerically, segment by segment', () => {
    expect(compareVersions('16.17.1', '9.24.1')).toBeGreaterThan(0);
    expect(compareVersions('9.24.1', '16.17.1')).toBeLessThan(0);
    expect(compareVersions('16.17.1', '16.17.1')).toBe(0);
    expect(compareVersions('16.2.1', '16.17.1')).toBeLessThan(0);
  });

  it('treats a missing segment as zero rather than as smaller', () => {
    expect(compareVersions('16.17', '16.17.0')).toBe(0);
    expect(compareVersions('16.17', '16.17.1')).toBeLessThan(0);
  });
});

describe('current version', () => {
  it('answers from Redis when it knows', async () => {
    await redis.set(VERSION_KEY, VERSION);
    expect(await currentVersion()).toBe(VERSION);
  });

  it('recovers the newest mirrored patch from disk when Redis has been flushed', async () => {
    // Redis is a cache of a fact that lives on disk; losing it must not make a
    // fully synced mirror read as never synced.
    await mkdir(join(mirror, '9.24.1'), { recursive: true });
    await mkdir(join(mirror, '10.5.2'), { recursive: true });
    await redis.del(VERSION_KEY);

    expect(await currentVersion()).toBe(VERSION);
    // And it writes the answer back rather than re-reading the directory.
    expect(await redis.get(VERSION_KEY)).toBe(VERSION);
  });
});

describe('syncing a patch', () => {
  it('writes every data file for the patch, plus the version list', async () => {
    const version = '16.18.1';
    publishPatch(version);
    await redis.del(VERSION_KEY);

    const result = await syncDdragon();

    expect(result).toMatchObject({ version, changed: true });
    expect(result.files).toEqual([...DATA_FILES]);
    expect(JSON.parse(await readFile(join(mirror, version, 'champion.json'), 'utf8'))).toEqual({
      file: 'champion',
      version,
    });
    // `/v1/static/versions` serves this, so it is part of the patch, not extra.
    expect(JSON.parse(await readFile(join(mirror, version, 'versions.json'), 'utf8'))).toEqual([
      version,
      VERSION,
    ]);
    expect(await redis.get(VERSION_KEY)).toBe(version);
  });

  it('is a no-op on the tick that finds the patch already mirrored (§10)', async () => {
    publishPatch('16.18.1');
    await redis.set(VERSION_KEY, '16.18.1');

    const result = await syncDdragon();

    expect(result).toEqual({ version: '16.18.1', changed: false, files: [] });
    // The version list, and nothing else: no data file was asked for.
    expect(cdn.requested).toEqual([VERSIONS_URL]);
  });

  it('re-downloads the same patch when forced', async () => {
    publishPatch('16.18.1');
    await redis.set(VERSION_KEY, '16.18.1');

    const result = await syncDdragon({ force: true });

    expect(result.changed).toBe(true);
    expect(cdn.requested).toContain(dataUrl('16.18.1', 'champion'));
  });

  it('skips a file Riot does not publish rather than losing the whole patch', async () => {
    // Riot has dropped data files over time, and a 403 on one of them used to be
    // the difference between a mirrored patch and no patch at all.
    const version = '16.19.1';
    publishPatch(version, ['champion', 'item']);
    await redis.del(VERSION_KEY);

    const result = await syncDdragon();

    expect(result.changed).toBe(true);
    expect(result.files).toEqual(['champion', 'item']);
    // The ones that did arrive are readable; the ones that did not are a miss.
    expect(await readStatic('champion', version)).toEqual({ file: 'champion', version });
    expect(await readStatic('map', version)).toBeUndefined();
  });
});

/**
 * The alias table is the route's whole public vocabulary, and #52 was an entry
 * on the other side of it naming a file the sync never mirrors. Nothing checked
 * that the two agreed.
 */
describe('the route’s file names', () => {
  it('aliases only files the sync actually mirrors', () => {
    for (const [alias, file] of Object.entries(FILE_ALIASES)) {
      expect(DATA_FILES, alias).toContain(file);
    }
  });

  it('gives every mirrored file a plural alias, so the vocabulary is uniform', () => {
    const aliased = new Set(Object.values(FILE_ALIASES));
    expect([...DATA_FILES].filter((file) => !aliased.has(file))).toEqual([]);
  });
});

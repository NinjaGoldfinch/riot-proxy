import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve, sep } from 'node:path';
import { request } from 'undici';
import { config } from '../config.js';
import { logger } from '../logger.js';
import { redis } from '../redis.js';

/**
 * §5.6 — Data Dragon is not rate limited and must never go through the limiter.
 * The worker syncs it to disk on a new patch; routes serve from disk (FR-11).
 */
export const DDRAGON_BASE = 'https://ddragon.leagueoflegends.com';
export const VERSIONS_URL = `${DDRAGON_BASE}/api/versions.json`;

/**
 * Riot's queue table (#52, #115). Not Data Dragon at all: a different host, no
 * version in the path, and no entry in `versions.json` — so it cannot live
 * under `DDRAGON_DIR/{version}/` without claiming a patch it does not have.
 *
 * It gets `DDRAGON_DIR/meta/` instead, which is the home this repo has owed
 * non-patch-versioned static data since #52 removed `queue` from the mirror's
 * file list for naming a file Data Dragon does not serve.
 */
export const QUEUES_URL = 'https://static.developer.riotgames.com/docs/lol/queues.json';

/** Where un-versioned static data lives, beside the per-patch directories. */
export const META_DIR = 'meta';

/** Un-versioned files mirrored into `META_DIR`. */
export const META_FILES = ['queues'] as const;

export type MetaFile = (typeof META_FILES)[number];

/** Data files mirrored per patch. Images stay on the CDN; §1 non-goals. */
export const DATA_FILES = [
  'champion',
  'item',
  'runesReforged',
  'summoner',
  'profileicon',
  'map',
] as const;

export type DataFile = (typeof DATA_FILES)[number];

const CURRENT_VERSION_KEY = 'ddragon:version';

/** What a mirrored patch directory is named: `16.17.1`, and nothing else. */
const VERSION_DIR = /^[0-9]+(\.[0-9]+)*$/;

export function ddragonDir(): string {
  return resolve(config.DDRAGON_DIR);
}

function versionDir(version: string): string {
  return join(ddragonDir(), version);
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await request(url, {
    method: 'GET',
    headers: { 'User-Agent': config.RIOT_USER_AGENT, Accept: 'application/json' },
    headersTimeout: 15_000,
    bodyTimeout: 60_000,
  });
  if (res.statusCode < 200 || res.statusCode >= 300) {
    await res.body.dump();
    throw new Error(`Data Dragon responded ${res.statusCode} for ${url}`);
  }
  return (await res.body.json()) as T;
}

export async function fetchVersions(): Promise<string[]> {
  return fetchJson<string[]>(VERSIONS_URL);
}

async function latestVersion(): Promise<string> {
  const versions = await fetchVersions();
  const latest = versions[0];
  if (!latest) throw new Error('Data Dragon returned an empty version list');
  return latest;
}

/** The version currently mirrored on disk, per Redis. */
export async function currentVersion(): Promise<string | undefined> {
  const cached = await redis.get(CURRENT_VERSION_KEY);
  if (cached) return cached;

  // Redis may have been flushed; recover from what is on disk.
  try {
    const entries = await readdir(ddragonDir(), { withFileTypes: true });
    const versions = entries
      .filter((e) => e.isDirectory())
      .map((e) => e.name)
      // Version directories only. `META_DIR` sits beside them and is not a
      // patch, and `compareVersions` parses segments with `Number` — so an
      // unfiltered list sorts a `NaN` into the answer and this returns `meta`
      // as the current version.
      .filter((name) => VERSION_DIR.test(name))
      .sort(compareVersions)
      .reverse();
    const found = versions[0];
    if (found) await redis.set(CURRENT_VERSION_KEY, found);
    return found;
  } catch {
    return undefined;
  }
}

export function compareVersions(a: string, b: string): number {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

export interface SyncResult {
  version: string;
  changed: boolean;
  files: string[];
  /** Un-versioned files refreshed this run — see `syncMeta`. */
  meta: string[];
}

/**
 * Refresh the un-versioned files, every run rather than only on a new patch.
 *
 * The patch check above is a *version* comparison, and these have no version:
 * Riot adds queue ids when a game mode ships, which is not the same event as a
 * patch landing in `versions.json`. Gating them on `changed` would mean a
 * deployment learning about Arena whenever the client next updated, for no
 * reason but that the two files happen to be fetched by the same job.
 *
 * A failure here is logged and swallowed, like a missing Data Dragon file: the
 * queue table is a labelling convenience, and losing it must not stop the
 * champion and item data a patch actually needs.
 */
async function syncMeta(): Promise<string[]> {
  const dir = join(ddragonDir(), META_DIR);
  await mkdir(dir, { recursive: true });

  const written: string[] = [];
  try {
    const queues = await fetchJson<unknown>(QUEUES_URL);
    await writeFile(join(dir, 'queues.json'), JSON.stringify(queues), 'utf8');
    written.push('queues');
  } catch (err) {
    logger.warn({ err, url: QUEUES_URL }, 'queue table unavailable, keeping what is on disk');
  }
  return written;
}

/**
 * Download this patch's data files to `DDRAGON_DIR/{version}/`. Idempotent:
 * re-running for a version already on disk is a no-op unless `force` is set.
 */
export async function syncDdragon(opts: { force?: boolean } = {}): Promise<SyncResult> {
  const version = await latestVersion();
  const known = await currentVersion();

  // Before the version check, not after it: these are not versioned, so
  // "nothing changed" is a statement about the patch and not about them.
  const meta = await syncMeta();

  if (!opts.force && known === version) {
    return { version, changed: false, files: [], meta };
  }

  const dir = versionDir(version);
  await mkdir(dir, { recursive: true });
  logger.info({ version }, 'syncing Data Dragon');

  const written: string[] = [];
  for (const file of DATA_FILES) {
    const url = `${DDRAGON_BASE}/cdn/${version}/data/${config.DDRAGON_LOCALE}/${file}.json`;
    try {
      const data = await fetchJson<unknown>(url);
      await writeFile(join(dir, `${file}.json`), JSON.stringify(data), 'utf8');
      written.push(file);
    } catch (err) {
      // A single missing data file (Riot has dropped some over time) must not
      // abort the whole patch sync.
      logger.warn({ err, file, version }, 'data dragon file unavailable, skipping');
    }
  }

  await writeFile(join(dir, 'versions.json'), JSON.stringify(await fetchVersions()), 'utf8');
  await redis.set(CURRENT_VERSION_KEY, version);

  logger.info({ version, files: written.length, meta: meta.length }, 'Data Dragon sync complete');
  return { version, changed: true, files: written, meta };
}

/**
 * Resolve `DDRAGON_DIR/{version}/{file}.json`, or undefined when either segment
 * would leave the mirror.
 *
 * `version` reaches here from a query string and `join` normalises `..`, so an
 * unchecked segment walks out of the directory. The route rejects anything that
 * is not a version number before we get here; this is the second guard, for the
 * other callers of an exported function, and it fails differently — the schema
 * rejects, this returns "not synced".
 */
function staticPath(version: string, file: string): string | undefined {
  const root = ddragonDir();
  const candidate = resolve(join(root, version, `${file}.json`));
  return candidate.startsWith(root + sep) ? candidate : undefined;
}

/**
 * Read an un-versioned mirrored file, or undefined when it has never synced.
 *
 * `META_DIR` is a literal rather than a caller's string, so this cannot be the
 * path-traversal shape `staticPath` guards against — it goes through the same
 * guard regardless, because a second reader of an exported helper is exactly
 * how that kind of hole gets reopened.
 */
export async function readMeta(file: MetaFile): Promise<unknown | undefined> {
  const path = staticPath(META_DIR, file);
  if (!path) return undefined;
  try {
    return JSON.parse(await readFile(path, 'utf8')) as unknown;
  } catch {
    return undefined;
  }
}

/** Read a mirrored file, or undefined when this patch was never synced. */
export async function readStatic(file: string, version?: string): Promise<unknown | undefined> {
  const v = version ?? (await currentVersion());
  if (!v) return undefined;
  const path = staticPath(v, file);
  if (!path) return undefined;
  try {
    const raw = await readFile(path, 'utf8');
    return JSON.parse(raw) as unknown;
  } catch {
    return undefined;
  }
}

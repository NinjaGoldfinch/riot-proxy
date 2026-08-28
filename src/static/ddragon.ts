import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
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

/** Data files mirrored per patch. Images stay on the CDN; §1 non-goals. */
export const DATA_FILES = [
  'champion',
  'item',
  'runesReforged',
  'summoner',
  'profileicon',
  'map',
  'queue',
] as const;

export type DataFile = (typeof DATA_FILES)[number];

const CURRENT_VERSION_KEY = 'ddragon:version';

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

export async function latestVersion(): Promise<string> {
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
}

/**
 * Download this patch's data files to `DDRAGON_DIR/{version}/`. Idempotent:
 * re-running for a version already on disk is a no-op unless `force` is set.
 */
export async function syncDdragon(opts: { force?: boolean } = {}): Promise<SyncResult> {
  const version = await latestVersion();
  const known = await currentVersion();

  if (!opts.force && known === version) {
    return { version, changed: false, files: [] };
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

  logger.info({ version, files: written.length }, 'Data Dragon sync complete');
  return { version, changed: true, files: written };
}

/** Read a mirrored file, or undefined when this patch was never synced. */
export async function readStatic(file: string, version?: string): Promise<unknown | undefined> {
  const v = version ?? (await currentVersion());
  if (!v) return undefined;
  try {
    const raw = await readFile(join(versionDir(v), `${file}.json`), 'utf8');
    return JSON.parse(raw) as unknown;
  } catch {
    return undefined;
  }
}

import { currentVersion, readStatic } from './ddragon.js';

/**
 * Champion id → display name, from the Data Dragon mirror (#111 of the
 * champion-stats plan). Cached in-process, keyed by the mirrored version:
 * `currentVersion()` is a Redis read, cheap enough to check on every call, but
 * `champion.json` is hundreds of champions of JSON and a per-request parse of
 * it would be the exact JSONB-open cost C1 spent an index removing elsewhere.
 * The cache only ever re-parses when the version it is keyed on has actually
 * moved — which `ddragon:sync` does at most once an hour (`DDRAGON_SYNC_S`).
 */

interface DdragonChampionEntry {
  key?: string;
  name?: string;
}

interface DdragonChampionFile {
  data?: Record<string, DdragonChampionEntry>;
}

let cache: { version: string; byId: Map<number, string> } | undefined;

async function championsByVersion(version: string): Promise<Map<number, string>> {
  if (cache?.version === version) return cache.byId;

  const file = (await readStatic('champion', version)) as DdragonChampionFile | undefined;
  const byId = new Map<number, string>();
  for (const entry of Object.values(file?.data ?? {})) {
    const id = Number(entry.key);
    if (Number.isFinite(id) && typeof entry.name === 'string') byId.set(id, entry.name);
  }

  cache = { version, byId };
  return byId;
}

/**
 * Names for a batch of champion ids — one version check and, at most, one
 * parse for however many rows a response carries. Ids the mirror does not
 * know (not yet synced, or a stale id from an old patch) are simply absent
 * from the result, matching the "absent means absent" rule the rest of the
 * proxy's Riot-shaped responses already follow.
 */
export async function championNames(championIds: number[]): Promise<Map<number, string>> {
  const version = await currentVersion();
  if (!version) return new Map();

  const byId = await championsByVersion(version);
  const result = new Map<number, string>();
  for (const id of championIds) {
    const name = byId.get(id);
    if (name) result.set(id, name);
  }
  return result;
}

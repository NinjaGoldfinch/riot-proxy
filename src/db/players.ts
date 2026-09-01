import { and, eq, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import { db, sql } from './index.js';
import { players, type Player } from './schema.js';

/**
 * Every read and write here is scoped by KEY_SCOPE (§7.4) — PUUIDs from a
 * different Riot key are a different namespace, not the same player.
 */

export interface UpsertPlayerInput {
  puuid: string;
  platform: string;
  gameName?: string | null;
  tagLine?: string | null;
  tracked?: boolean;
  lastSeenMatchId?: string | null;
}

/**
 * Absent fields are left as they are rather than overwritten. The lookup path
 * calls this with nothing but a PUUID and a platform, and must not blank the
 * Riot ID an admin track put there — so only what the caller actually passed
 * is written. Passing an explicit `null` still clears the column.
 */
export async function upsertPlayer(input: UpsertPlayerInput): Promise<Player> {
  const provided = {
    ...(input.gameName !== undefined ? { gameName: input.gameName } : {}),
    ...(input.tagLine !== undefined ? { tagLine: input.tagLine } : {}),
    ...(input.tracked !== undefined ? { tracked: input.tracked } : {}),
    ...(input.lastSeenMatchId !== undefined ? { lastSeenMatchId: input.lastSeenMatchId } : {}),
  };

  const [row] = await db
    .insert(players)
    .values({
      puuid: input.puuid,
      keyScope: KEY_SCOPE,
      platform: input.platform,
      ...provided,
      updatedAt: new Date(),
    })
    .onConflictDoUpdate({
      target: [players.keyScope, players.puuid],
      set: { platform: input.platform, ...provided, updatedAt: new Date() },
    })
    .returning();

  if (!row) throw new Error('failed to upsert player');
  return row;
}

/**
 * The ladder crawl's version of `upsertPlayer`: a page of them at once, and
 * deliberately narrower.
 *
 * Narrower because of the one rule discovery must not break — **never
 * auto-track**. `tracked = true` means a 60-second spectator poll, and a crawl
 * turning it on for thousands of players would drown the limiter in exactly the
 * traffic the tier floors exist to bound. The conflict clause here touches only
 * `platform` and `updated_at`, so the invariant is the database's rather than a
 * caller's to remember; the insert relies on the column default for new rows.
 *
 * A page at a time because a Riot page is ~205 players and `upsertPlayer` is
 * one round trip apiece. The rows come back so the caller can read
 * `history_backfill_started_at` without a second query — which is how a repeat
 * crawl knows whom it has already walked.
 */
export async function upsertDiscoveredPlayers(
  discovered: { puuid: string; platform: string }[],
): Promise<Player[]> {
  const rows: Player[] = [];
  const now = new Date();

  // Same batch size and reasoning as `upsertLeagueEntries` and the archive
  // helpers: comfortably inside Postgres' bind-parameter ceiling.
  for (let i = 0; i < discovered.length; i += 100) {
    const batch = discovered.slice(i, i + 100);
    const written = await db
      .insert(players)
      .values(
        batch.map((p) => ({
          puuid: p.puuid,
          keyScope: KEY_SCOPE,
          platform: p.platform,
          updatedAt: now,
        })),
      )
      .onConflictDoUpdate({
        target: [players.keyScope, players.puuid],
        set: { platform: raw`excluded.platform`, updatedAt: now },
      })
      .returning();
    rows.push(...written);
  }

  return rows;
}

export async function getPlayer(puuid: string): Promise<Player | undefined> {
  const rows = await db
    .select()
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)))
    .limit(1);
  return rows[0];
}

export async function listTrackedPlayers(): Promise<Player[]> {
  return db
    .select()
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.tracked, true)));
}

export async function listPlayers(): Promise<Player[]> {
  return db.select().from(players).where(eq(players.keyScope, KEY_SCOPE));
}

/**
 * How many players this key scope is tracking. `listPlayers().filter(...)`
 * answered the same question by pulling every row across the wire to produce
 * one number; `countArchivedMatches` next door already had the right shape.
 */
export async function countTrackedPlayers(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.tracked, true)));
  return rows[0]?.n ?? 0;
}

/** Every player ever seen under this key scope, tracked or not. */
export async function countPlayers(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(players)
    .where(eq(players.keyScope, KEY_SCOPE));
  return rows[0]?.n ?? 0;
}

export async function setTracked(puuid: string, tracked: boolean): Promise<boolean> {
  const rows = await db
    .update(players)
    .set({ tracked, updatedAt: new Date() })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)))
    .returning({ puuid: players.puuid });
  return rows.length > 0;
}

export async function setLastSeenMatch(puuid: string, matchId: string): Promise<void> {
  await db
    .update(players)
    .set({ lastSeenMatchId: matchId, updatedAt: new Date() })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)));
}

/**
 * #44 — a walk is starting. Upserts, because the admin backfill route can name
 * a player nobody has looked up yet, and the stamp needs a row to live on.
 *
 * Recording the start rather than only the finish is what makes a walk that
 * died mid-way distinguishable from one that never ran.
 */
export async function markBackfillStarted(puuid: string, platform: string): Promise<void> {
  const now = new Date();
  await db
    .insert(players)
    .values({
      puuid,
      keyScope: KEY_SCOPE,
      platform,
      historyBackfillStartedAt: now,
      updatedAt: now,
    })
    .onConflictDoUpdate({
      target: [players.keyScope, players.puuid],
      set: { historyBackfillStartedAt: now, updatedAt: now },
    });
}

/**
 * The walk reached the end of the history (or its limit). `depth` is how many
 * matches back it actually got, so a limit raised later can be told apart from
 * a history that simply ran out.
 */
export async function markBackfillComplete(puuid: string, depth: number): Promise<void> {
  const now = new Date();
  await db
    .update(players)
    .set({ historyBackfilledAt: now, historyBackfillDepth: depth, updatedAt: now })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)));
}

/**
 * Fill in Riot IDs from the archive, without touching Riot (#108 follow-up).
 *
 * A crawl enumerates PUUIDs and nothing else — `league-v4` stopped carrying
 * summoner names — so every player it discovers arrives nameless, and the
 * dashboard draws thirty rows of truncated base64. Asking `account-v1` who
 * each one is would cost one request per player, which on a full ladder is a
 * second crawl's worth of quota spent on cosmetics, competing with the match
 * fetches that are the only thing a crawl can't get anywhere else.
 *
 * It is also unnecessary, because match-v5 already answers. Every participant
 * in an archived match carries `riotIdGameName`/`riotIdTagline`, the archive
 * stage is storing those matches anyway, and `matches.data` is the raw Riot
 * body — so the names are already in Postgres. This reads them back out. No
 * upstream call, no quota, nothing that can 429.
 *
 * What it is accurate to is worth being precise about: a name is what the
 * player was called *during that game*, not necessarily now. So it takes the
 * most recent match it has, and only ever fills a `NULL` — a row already
 * holding a name got it from `account-v1` (via `rememberIdentity`) or from an
 * admin, and both of those are more authoritative than a game from last week.
 * A rename therefore corrects itself the next time anyone views the profile,
 * rather than being fought over by two writers.
 *
 * The three-match window is for the matches where Riot returns the Riot ID
 * fields empty. Looking only at the newest match would leave those players
 * nameless permanently — the target set is deterministic, so every later run
 * would examine the same empty match and give up again.
 *
 * `limit` is a safety valve rather than a page: a target with nothing archived
 * costs an empty index scan on `match_participants_puuid_idx`, and one that is
 * named drops out of the target set for good, so a full pass converges instead
 * of re-doing work.
 */
export async function backfillNamesFromArchive(
  limit = 50_000,
): Promise<{ named: number; unnamed: number }> {
  const named = await sql<{ puuid: string }[]>`
    with targets as (
      select puuid
      from players
      where key_scope = ${KEY_SCOPE} and game_name is null
      -- Only matters when the valve bites, and then the freshest crawl is the
      -- one someone is watching.
      order by updated_at desc
      limit ${limit}
    ),
    recent as (
      select t.puuid, r.match_id, r.game_end_ts
      from targets t
      cross join lateral (
        select m.match_id, m.game_end_ts
        from match_participants mp
        join matches m on m.match_id = mp.match_id
        where mp.puuid = t.puuid
        order by m.game_end_ts desc nulls last, m.match_id desc
        limit 3
      ) r
    ),
    resolved as (
      select distinct on (r.puuid)
        r.puuid,
        r.game_end_ts,
        r.match_id,
        -- riotIdName is what matches from the Riot ID transition call the
        -- same field. summonerName is deliberately not a fallback: it is the
        -- pre-Riot-ID display name, and a name with no tag is not the identity
        -- anything downstream looks a player up by.
        coalesce(
          nullif(part->>'riotIdGameName', ''),
          nullif(part->>'riotIdName', '')
        ) as game_name,
        nullif(part->>'riotIdTagline', '') as tag_line
      from recent r
      join matches m on m.match_id = r.match_id
      -- A body without an array here would make jsonb_array_elements raise
      -- rather than return nothing, and a WHERE guard runs too late to stop it.
      cross join lateral jsonb_array_elements(
        case
          when jsonb_typeof(m.data->'info'->'participants') = 'array'
          then m.data->'info'->'participants'
          else '[]'::jsonb
        end
      ) part
      where part->>'puuid' = r.puuid
        and coalesce(
          nullif(part->>'riotIdGameName', ''),
          nullif(part->>'riotIdName', '')
        ) is not null
      order by r.puuid, r.game_end_ts desc nulls last, r.match_id desc
    )
    update players p
    set game_name = resolved.game_name,
        tag_line = resolved.tag_line,
        updated_at = now()
    from resolved
    where p.key_scope = ${KEY_SCOPE}
      and p.puuid = resolved.puuid
      -- Re-checked here, not only in the targets CTE: that is a snapshot, and
      -- a profile lookup landing mid-statement must not be overwritten by it.
      and p.game_name is null
    returning p.puuid
  `;

  return { named: named.length, unnamed: await countUnnamedPlayers() };
}

/** How many players this key scope still holds nothing but a PUUID for. */
export async function countUnnamedPlayers(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), raw`${players.gameName} is null`));
  return rows[0]?.n ?? 0;
}

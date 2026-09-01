import { Type } from '@sinclair/typebox';
import type { FastifyPluginAsync } from 'fastify';
import { config } from '../config.js';
import {
  latestPatch,
  listAnalyticsSlices,
  listChampionBans,
  listChampionItems,
  listChampionMatchups,
  listChampionRunes,
  listChampionSpells,
  listChampionStats,
  type ChampionStatRow,
} from '../db/analytics.js';
import { ProxyError } from '../errors.js';
import { fetcher } from '../fetcher.js';
import { build } from '../riot/endpoints.js';
import {
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
} from '../riot/ladder.js';
import { assertPlatform, assertRegion, regionFromMatchId } from '../riot/routing.js';
import { championNames } from '../static/champions.js';
import { send } from './helpers.js';
import {
  ApexTierParam,
  ChampionDetailQuery,
  ChampionDetailResponse,
  ChampionIdParam,
  ChampionMatchupsQuery,
  ChampionMatchupsResponse,
  ChampionStatsQuery,
  ChampionStatsResponse,
  DivisionParam,
  LadderPageQuery,
  LadderTierParam,
  MasteryQuery,
  MatchIdParam,
  MatchIdsQuery,
  PassthroughResponse,
  PlatformParam,
  PuuidParam,
  RankedQueueParam,
  RegionParam,
  localErrors,
  upstreamErrors,
} from './schemas.js';

/** §6.2 — the LoL surface. Every handler is a thin shell over `fetcher.fetch`. */
const lolRoutes: FastifyPluginAsync = async (fastify) => {
  fastify.get(
    '/v1/lol/summoners/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      return send(
        reply,
        await fetcher.fetch(build.summonerByPuuid(assertPlatform(platform), puuid)),
      );
    },
  );

  fastify.get(
    '/v1/lol/league/entries/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      return send(
        reply,
        await fetcher.fetch(build.leagueEntriesByPuuid(assertPlatform(platform), puuid)),
      );
    },
  );

  /**
   * The ladder, in the two shapes Riot serves it. Both are passthroughs like
   * everything else here; the crawl (#88) drives the same builders from the
   * worker at bulk priority rather than through these routes.
   */
  fastify.get(
    '/v1/lol/league/apex/:platform/:tier/:queue',
    {
      schema: {
        tags: ['lol'],
        summary: 'Read a whole apex league',
        params: Type.Object({
          platform: PlatformParam,
          tier: ApexTierParam,
          queue: RankedQueueParam,
        }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, tier, queue } = request.params as {
        platform: string;
        tier: string;
        queue: string;
      };
      return send(
        reply,
        await fetcher.fetch(
          build.apexLeague(
            assertPlatform(platform),
            assertApexTier(tier),
            assertRankedQueue(queue),
          ),
        ),
      );
    },
  );

  fastify.get(
    '/v1/lol/league/entries/:platform/:queue/:tier/:division',
    {
      schema: {
        tags: ['lol'],
        summary: 'Walk one page of a tier and division',
        description:
          'One ~205-entry page of the ladder. Pages are 1-based; a page past the end of the ' +
          'division returns an empty array rather than a 404.',
        params: Type.Object({
          platform: PlatformParam,
          queue: RankedQueueParam,
          tier: LadderTierParam,
          division: DivisionParam,
        }),
        querystring: LadderPageQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, queue, tier, division } = request.params as {
        platform: string;
        queue: string;
        tier: string;
        division: string;
      };
      const { page } = request.query as { page?: number };
      return send(
        reply,
        await fetcher.fetch(
          build.leagueEntriesByTier(
            assertPlatform(platform),
            assertRankedQueue(queue),
            assertPagedTier(tier),
            assertDivision(division),
            page,
          ),
        ),
      );
    },
  );

  fastify.get(
    '/v1/lol/matches/ids/:region/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, puuid: PuuidParam }),
        querystring: MatchIdsQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, puuid } = request.params as { region: string; puuid: string };
      const query = request.query as Record<string, number | string | undefined>;
      return send(
        reply,
        await fetcher.fetch(build.matchIdsByPuuid(assertRegion(region), puuid, query)),
      );
    },
  );

  /**
   * Match + timeline are immutable: `fetcher` checks the Postgres archive
   * before Redis or Riot (Phase 5), so an archived match costs zero quota.
   */
  fastify.get(
    '/v1/lol/matches/:region/:matchId',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, matchId: MatchIdParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, matchId } = request.params as { region: string; matchId: string };
      return send(
        reply,
        await fetcher.fetch(build.matchById(resolveMatchRegion(region, matchId), matchId)),
      );
    },
  );

  fastify.get(
    '/v1/lol/matches/:region/:matchId/timeline',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ region: RegionParam, matchId: MatchIdParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { region, matchId } = request.params as { region: string; matchId: string };
      return send(
        reply,
        await fetcher.fetch(build.matchTimeline(resolveMatchRegion(region, matchId), matchId)),
      );
    },
  );

  fastify.get(
    '/v1/lol/spectator/active/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      // A 404 here means "not in game" and is negative-cached for 30 s (§8.3).
      return send(reply, await fetcher.fetch(build.activeGame(assertPlatform(platform), puuid)));
    },
  );

  fastify.get(
    '/v1/lol/mastery/by-puuid/:platform/:puuid',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam, puuid: PuuidParam }),
        querystring: MasteryQuery,
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform, puuid } = request.params as { platform: string; puuid: string };
      const { top } = request.query as { top?: number };
      const p = assertPlatform(platform);
      const req = top ? build.masteryTopByPuuid(p, puuid, top) : build.masteryByPuuid(p, puuid);
      return send(reply, await fetcher.fetch(req));
    },
  );

  fastify.get(
    '/v1/lol/rotations/:platform',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.championRotations(assertPlatform(platform))));
    },
  );

  /**
   * What the crawl is for: pick and win rates per champion, at a tier.
   *
   * The one route in this file that never touches Riot — it reads
   * `champion_stats`, which a `maintenance` job recomputes from the archive
   * after a crawl completes. Empty until a crawl has run and been aggregated,
   * which is the honest answer rather than a 404: the endpoint exists, the
   * numbers do not yet.
   */
  fastify.get(
    '/v1/lol/analytics/champions',
    {
      schema: {
        tags: ['lol'],
        summary: 'Champion pick and win rates by tier',
        description:
          'Aggregated from the match archive, with each participant placed at the tier the ' +
          'latest ladder crawl found them at. Recomputed per (platform, queue) when a crawl ' +
          'completes.',
        querystring: ChampionStatsQuery,
        response: { 200: ChampionStatsResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const query = request.query as {
        platform?: string;
        queue?: string;
        tier?: string;
        patch?: string;
        role?: string;
        minGames?: number;
        limit?: number;
      };
      const platform = assertPlatform(query.platform ?? config.DEFAULT_PLATFORM);
      const queue = assertRankedQueue(query.queue ?? config.ladderQueues[0] ?? 'RANKED_SOLO_5x5');
      const tier = query.tier ? assertTier(query.tier) : undefined;
      // Newest aggregated patch by default: asking for "champion win rates"
      // without a patch means the current one, not every patch ever archived
      // averaged into a single meaningless number.
      const patch = query.patch ?? (await latestPatch(platform, queue));
      const minGames = query.minGames ?? config.AGGREGATE_MIN_GAMES;

      // Three independent lookups off the same slice rather than a join: none
      // of them shares a grain with `champion_stats` — a slice is one row per
      // tier, a ban is one row per (tier, champion), and neither carries a
      // role — so folding them into one query would fan champion_stats' rows
      // out and back in for nothing.
      const [rows, slices, bans] = patch
        ? await Promise.all([
            listChampionStats({
              platform,
              queue,
              patch,
              minGames,
              ...(tier ? { tier } : {}),
              ...(query.role !== undefined ? { role: query.role } : {}),
              ...(query.limit ? { limit: query.limit } : {}),
            }),
            listAnalyticsSlices({ platform, queue, patch, ...(tier ? { tier } : {}) }),
            listChampionBans({ platform, queue, patch, ...(tier ? { tier } : {}) }),
          ])
        : [[], [], []];

      const names = await championNames(rows.map((r) => r.championId));
      const totalGames = rows.reduce((total, row) => total + row.games, 0);
      // A recompute writes the whole slice at once, so any row's stamp is the
      // slice's; the newest is taken in case a partial write is ever visible.
      const computedAt = rows.reduce<Date | null>(
        (newest, row) => (!newest || row.computedAt > newest ? row.computedAt : newest),
        null,
      );

      // Derived from immutable archive rows, and only ever replaced wholesale
      // by a recompute — so it is safe to hold, and holding it is what keeps a
      // dashboard polling this off the database.
      reply.header('Cache-Control', 'public, max-age=300');

      return {
        platform,
        queue,
        tier: tier ?? null,
        patch: patch ?? null,
        role: query.role ?? null,
        computedAt: computedAt ? computedAt.toISOString() : null,
        totalGames,
        champions: enrichChampionStats(rows, slices, bans, names),
      };
    },
  );

  /**
   * One champion's lane matchups (#112). Its own route rather than a filter
   * on the aggregate above: `champion_matchups` has a different grain
   * (patch/role/champion/opponent, no tier) — folding it in would mean every
   * caller of the plain champion list paying for a self-join it never asked
   * for.
   */
  fastify.get(
    '/v1/lol/analytics/champions/:championId/matchups',
    {
      schema: {
        tags: ['lol'],
        summary: "A champion's lane matchups",
        description:
          'Both directions of every lane matchup this champion has archived data for. No tier ' +
          "dimension: sample sizes die fast enough per (champion, opponent, role) alone, and " +
          'the two laners can sit in different tiers anyway.',
        params: Type.Object({ championId: ChampionIdParam }),
        querystring: ChampionMatchupsQuery,
        response: { 200: ChampionMatchupsResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const { championId } = request.params as { championId: number };
      const query = request.query as {
        platform?: string;
        queue?: string;
        patch?: string;
        role?: string;
        minGames?: number;
        limit?: number;
      };
      const platform = assertPlatform(query.platform ?? config.DEFAULT_PLATFORM);
      const queue = assertRankedQueue(query.queue ?? config.ladderQueues[0] ?? 'RANKED_SOLO_5x5');
      const patch = query.patch ?? (await latestPatch(platform, queue));

      const rows = patch
        ? await listChampionMatchups({
            platform,
            queue,
            patch,
            championId,
            ...(query.role !== undefined ? { role: query.role } : {}),
            ...(query.minGames !== undefined ? { minGames: query.minGames } : {}),
            ...(query.limit ? { limit: query.limit } : {}),
          })
        : [];

      const names = await championNames([championId, ...rows.map((r) => r.opponentId)]);

      reply.header('Cache-Control', 'public, max-age=300');

      return {
        championId,
        ...(names.has(championId) ? { championName: names.get(championId) } : {}),
        platform,
        queue,
        patch: patch ?? null,
        role: query.role ?? null,
        matchups: rows.map((row) => ({
          role: row.role,
          opponentId: row.opponentId,
          ...(names.has(row.opponentId) ? { opponentName: names.get(row.opponentId) } : {}),
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
        })),
      };
    },
  );

  /**
   * One call for a champion page (#112): the stat row(s) at this slice, and
   * this champion's top matchups/items/runes/spells. Each section is its own
   * lookup — none shares a grain with any other — run concurrently rather
   * than joined, the same reasoning as the plain champion list above.
   */
  fastify.get(
    '/v1/lol/analytics/champions/:championId',
    {
      schema: {
        tags: ['lol'],
        summary: 'Champion detail composite',
        description:
          "The stat row(s) at this slice plus this champion's top lane matchups, items, runes " +
          'and summoner spells — one call for a champion page. Each section is independently ' +
          'trimmed by `minGames`/`limit`; a champion nobody has data for yet still returns 200 ' +
          'with empty arrays rather than a 404.',
        params: Type.Object({ championId: ChampionIdParam }),
        querystring: ChampionDetailQuery,
        response: { 200: ChampionDetailResponse, ...localErrors },
      },
    },
    async (request, reply) => {
      const { championId } = request.params as { championId: number };
      const query = request.query as {
        platform?: string;
        queue?: string;
        tier?: string;
        patch?: string;
        role?: string;
        minGames?: number;
        limit?: number;
      };
      const platform = assertPlatform(query.platform ?? config.DEFAULT_PLATFORM);
      const queue = assertRankedQueue(query.queue ?? config.ladderQueues[0] ?? 'RANKED_SOLO_5x5');
      const tier = query.tier ? assertTier(query.tier) : undefined;
      const patch = query.patch ?? (await latestPatch(platform, queue));
      const role = query.role !== undefined ? { role: query.role } : {};
      const minGames = query.minGames ?? 0;
      const limit = query.limit ? { limit: query.limit } : {};

      const [stats, slices, bans, matchups, items, runes, spells] = patch
        ? await Promise.all([
            listChampionStats({ platform, queue, patch, championId, ...(tier ? { tier } : {}), ...role }),
            listAnalyticsSlices({ platform, queue, patch, ...(tier ? { tier } : {}) }),
            listChampionBans({ platform, queue, patch, ...(tier ? { tier } : {}) }),
            listChampionMatchups({ platform, queue, patch, championId, ...role, minGames, ...limit }),
            listChampionItems({ platform, queue, patch, championId, ...role, minGames, ...limit }),
            listChampionRunes({ platform, queue, patch, championId, ...role, minGames, ...limit }),
            listChampionSpells({ platform, queue, patch, championId, ...role, minGames, ...limit }),
          ])
        : [[], [], [], [], [], [], []];

      const names = await championNames([championId, ...matchups.map((m) => m.opponentId)]);

      reply.header('Cache-Control', 'public, max-age=300');

      return {
        championId,
        ...(names.has(championId) ? { championName: names.get(championId) } : {}),
        platform,
        queue,
        patch: patch ?? null,
        role: query.role ?? null,
        stats: enrichChampionStats(stats, slices, bans, names),
        matchups: matchups.map((row) => ({
          role: row.role,
          opponentId: row.opponentId,
          ...(names.has(row.opponentId) ? { opponentName: names.get(row.opponentId) } : {}),
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
        })),
        items: items.map((row) => ({
          itemId: row.itemId,
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
        })),
        runes: runes.map((row) => ({
          keystoneId: row.keystoneId,
          subStyleId: row.subStyleId,
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
        })),
        spells: spells.map((row) => ({
          spellA: row.spellA,
          spellB: row.spellB,
          games: row.games,
          wins: row.wins,
          winRate: round(row.wins / row.games),
        })),
      };
    },
  );

  fastify.get(
    '/v1/lol/status/:platform',
    {
      schema: {
        tags: ['lol'],
        params: Type.Object({ platform: PlatformParam }),
        response: { 200: PassthroughResponse, ...upstreamErrors },
      },
    },
    async (request, reply) => {
      const { platform } = request.params as { platform: string };
      return send(reply, await fetcher.fetch(build.platformStatus(assertPlatform(platform))));
    },
  );
};

/** Four decimal places: enough for a win rate, short of implying precision. */
function round(value: number): number {
  return Math.round(value * 10_000) / 10_000;
}

/**
 * `champion_stats` rows plus their slice/ban/name context, turned into the
 * response shape `ChampionStatEntry` describes. Shared by the champion list
 * route and the detail composite's `stats` section rather than duplicated —
 * `pickRate`/`banRate` already had two real bugs caught in review once, and a
 * second implementation is a second place either could drift back in.
 *
 * `totalGames`/`share` are computed over exactly the rows passed in: the list
 * route passes a whole slice, so `share` means "of the slice"; the detail
 * composite passes one champion's rows across tiers, so it means "of this
 * champion's own games in this slice" — both are the same formula, applied
 * to a different `rows` set by the caller.
 */
function enrichChampionStats(
  rows: ChampionStatRow[],
  slices: { tier: string; matches: number }[],
  bans: { tier: string; championId: number; bans: number }[],
  names: Map<number, string>,
) {
  const sliceMatches = new Map(slices.map((s) => [s.tier, s.matches]));
  const banCounts = new Map(bans.map((b) => [`${b.tier}:${b.championId}`, b.bans]));
  const totalGames = rows.reduce((total, row) => total + row.games, 0);

  return rows.map((row) => {
    const slice = sliceMatches.get(row.tier);
    // Absent from `champion_bans` means a computed zero, not an unknown —
    // the recompute writes both tables from the same transaction, so a
    // champion with no ban row in a slice that exists was simply never
    // banned in it (§6.3 of the plan).
    const bansForChampion = banCounts.get(`${row.tier}:${row.championId}`) ?? 0;
    const minutes = row.durationS / 60;
    return {
      championId: row.championId,
      ...(names.has(row.championId) ? { championName: names.get(row.championId) } : {}),
      tier: row.tier,
      patch: row.patch,
      games: row.games,
      wins: row.wins,
      winRate: round(row.wins / row.games),
      share: totalGames > 0 ? round(row.games / totalGames) : 0,
      // `matchesPicked` is stored per role and summed across roles when none
      // is filtered — correct within one role (the primary key guarantees
      // one row), but a champion picked in two different roles within the
      // *same* match sums that match twice. Clamped rather than chasing an
      // exact cross-role distinct count, which would mean re-opening
      // `match_participants` per request — the JSONB-open cost this whole
      // feature exists to avoid.
      ...(slice ? { pickRate: round(Math.min(1, row.matchesPicked / slice)) } : {}),
      ...(slice ? { banRate: round(bansForChampion / slice) } : {}),
      ...(row.statedGames > 0
        ? {
            avgKda: round((row.kills + row.assists) / Math.max(row.deaths, 1)),
            avgDamage: round(row.damage / row.statedGames),
            avgVision: round(row.vision / row.statedGames),
          }
        : {}),
      ...(minutes > 0
        ? { csPerMin: round(row.cs / minutes), goldPerMin: round(row.gold / minutes) }
        : {}),
    };
  });
}

/**
 * Match IDs carry their own platform prefix (`EUW1_…`). When the caller's
 * region disagrees with the ID, the ID wins — routing a match to the wrong
 * regional host is a guaranteed 404.
 */
function resolveMatchRegion(region: string, matchId: string) {
  const declared = assertRegion(region);
  const derived = regionFromMatchId(matchId);
  if (derived && derived !== declared) {
    throw ProxyError.badRegion(
      `Match ${matchId} belongs to region '${derived}', not '${declared}'`,
    );
  }
  return derived ?? declared;
}

export default lolRoutes;

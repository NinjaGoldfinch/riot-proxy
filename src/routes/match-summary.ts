import { Type } from '@sinclair/typebox';

/**
 * The overview projection for the composite match page (§6.3).
 *
 * A match-v5 payload carries ten participants of ~130 fields each, a
 * `challenges` object of ~100 more per participant, plus team objects, bans and
 * rune trees — tens of kilobytes to say who won and on which champion. A page
 * of ten of those is around a megabyte of response for a panel that renders a
 * champion icon, a scoreline and six items. So the page returns this instead:
 * the requesting player's line in each game, and nothing else.
 *
 * None of it is lost. Every match is still archived whole (§7.3), and
 * `GET /v1/lol/matches/{region}/{matchId}` serves the full document from
 * Postgres at zero upstream cost — which is where a caller goes when it wants
 * the other nine players, the timeline, or a field not listed here.
 *
 * Two rules keep this honest:
 *
 *   1. Riot's own field names and values, verbatim. Nothing is renamed and
 *      nothing is computed — no KDA ratio, no CS/min. A caller that outgrows
 *      the summary and moves to the full match changes its source, not a single
 *      field name. `perks` is the one exception, and it is a subset rather than
 *      a rewrite (below).
 *   2. Absent means absent. Riot omits fields by patch, queue and match age, so
 *      everything but `matchId` is optional and a value we cannot read is left
 *      out rather than nulled.
 *
 * The schema below is the response schema Fastify serialises with, which is
 * what makes it structurally impossible for a full payload to leak back into
 * this endpoint. It also means a field added to `summariseMatch` and not to the
 * schema is dropped silently: the two halves of this file are one change.
 */

/** Riot's `perks` blob is ~40 lines to convey three icon IDs. */
const PerksSummarySchema = Type.Object({
  keystone: Type.Optional(Type.Number()),
  primaryStyle: Type.Optional(Type.Number()),
  subStyle: Type.Optional(Type.Number()),
});

const PlayerSummarySchema = Type.Object({
  puuid: Type.Optional(Type.String()),
  win: Type.Optional(Type.Boolean()),
  /** A remake: rendered as neither a win nor a loss. */
  gameEndedInEarlySurrender: Type.Optional(Type.Boolean()),
  championId: Type.Optional(Type.Number()),
  championName: Type.Optional(Type.String()),
  champLevel: Type.Optional(Type.Number()),
  teamId: Type.Optional(Type.Number()),
  teamPosition: Type.Optional(Type.String()),
  kills: Type.Optional(Type.Number()),
  deaths: Type.Optional(Type.Number()),
  assists: Type.Optional(Type.Number()),
  totalMinionsKilled: Type.Optional(Type.Number()),
  neutralMinionsKilled: Type.Optional(Type.Number()),
  goldEarned: Type.Optional(Type.Number()),
  visionScore: Type.Optional(Type.Number()),
  totalDamageDealtToChampions: Type.Optional(Type.Number()),
  item0: Type.Optional(Type.Number()),
  item1: Type.Optional(Type.Number()),
  item2: Type.Optional(Type.Number()),
  item3: Type.Optional(Type.Number()),
  item4: Type.Optional(Type.Number()),
  item5: Type.Optional(Type.Number()),
  item6: Type.Optional(Type.Number()),
  summoner1Id: Type.Optional(Type.Number()),
  summoner2Id: Type.Optional(Type.Number()),
  perks: Type.Optional(PerksSummarySchema),
  /** Arena (queue 1700): there is no win/loss to render, only a placement. */
  placement: Type.Optional(Type.Number()),
  playerSubteamId: Type.Optional(Type.Number()),
});

export const MatchSummarySchema = Type.Object(
  {
    matchId: Type.String(),
    queueId: Type.Optional(Type.Number()),
    gameMode: Type.Optional(Type.String()),
    /** Which patch this was played on — the Data Dragon version to render it. */
    gameVersion: Type.Optional(Type.String()),
    gameCreation: Type.Optional(Type.Number()),
    gameEndTimestamp: Type.Optional(Type.Number()),
    gameDuration: Type.Optional(Type.Number()),
    endOfGameResult: Type.Optional(Type.String()),
    player: Type.Optional(PlayerSummarySchema),
  },
  { $id: 'MatchSummary' },
);

interface PerksSummary {
  keystone?: number;
  primaryStyle?: number;
  subStyle?: number;
}

export interface PlayerSummary {
  puuid?: string;
  win?: boolean;
  gameEndedInEarlySurrender?: boolean;
  championId?: number;
  championName?: string;
  champLevel?: number;
  teamId?: number;
  teamPosition?: string;
  kills?: number;
  deaths?: number;
  assists?: number;
  totalMinionsKilled?: number;
  neutralMinionsKilled?: number;
  goldEarned?: number;
  visionScore?: number;
  totalDamageDealtToChampions?: number;
  item0?: number;
  item1?: number;
  item2?: number;
  item3?: number;
  item4?: number;
  item5?: number;
  item6?: number;
  summoner1Id?: number;
  summoner2Id?: number;
  perks?: PerksSummary;
  placement?: number;
  playerSubteamId?: number;
}

export interface MatchSummary {
  matchId: string;
  queueId?: number;
  gameMode?: string;
  gameVersion?: string;
  gameCreation?: number;
  gameEndTimestamp?: number;
  gameDuration?: number;
  endOfGameResult?: string;
  player?: PlayerSummary;
}

type Unknowns = Record<string, unknown>;

const obj = (value: unknown): Unknowns | undefined =>
  typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Unknowns)
    : undefined;
const num = (value: unknown): number | undefined =>
  typeof value === 'number' && Number.isFinite(value) ? value : undefined;
const str = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined);
const bool = (value: unknown): boolean | undefined =>
  typeof value === 'boolean' ? value : undefined;

/** Drop the keys we could not read, so absent stays absent rather than null. */
function compact<T extends object>(value: T): T {
  for (const key of Object.keys(value) as (keyof T)[]) {
    if (value[key] === undefined) delete value[key];
  }
  return value;
}

/**
 * Project one match down to the requesting player's line in it.
 *
 * `matchId` is taken from the payload when it is there and from the id we asked
 * for otherwise: it is the one field a caller needs in order to fetch the rest,
 * so it must never be missing.
 *
 * Returns `null` when the payload holds no participant for this PUUID — a
 * mismatched archive row, or a match that came back malformed. The caller drops
 * those into `warnings[]` rather than serving a summary about nobody.
 */
export function summariseMatch(
  match: unknown,
  puuid: string,
  requestedMatchId: string,
): MatchSummary | null {
  const root = obj(match);
  if (!root) return null;

  const info = obj(root.info);
  const metadata = obj(root.metadata);
  const participants = Array.isArray(info?.participants) ? info.participants : [];
  const player = participants
    .map((p) => obj(p))
    .find((p): p is Unknowns => p !== undefined && p.puuid === puuid);
  if (!player) return null;

  return compact({
    matchId: str(metadata?.matchId) ?? requestedMatchId,
    queueId: num(info?.queueId),
    gameMode: str(info?.gameMode),
    gameVersion: str(info?.gameVersion),
    gameCreation: num(info?.gameCreation),
    gameEndTimestamp: num(info?.gameEndTimestamp),
    gameDuration: num(info?.gameDuration),
    endOfGameResult: str(info?.endOfGameResult),
    player: summarisePlayer(player),
  });
}

function summarisePlayer(player: Unknowns): PlayerSummary {
  return compact({
    puuid: str(player.puuid),
    win: bool(player.win),
    gameEndedInEarlySurrender: bool(player.gameEndedInEarlySurrender),
    championId: num(player.championId),
    championName: str(player.championName),
    champLevel: num(player.champLevel),
    teamId: num(player.teamId),
    teamPosition: str(player.teamPosition),
    kills: num(player.kills),
    deaths: num(player.deaths),
    assists: num(player.assists),
    totalMinionsKilled: num(player.totalMinionsKilled),
    neutralMinionsKilled: num(player.neutralMinionsKilled),
    goldEarned: num(player.goldEarned),
    visionScore: num(player.visionScore),
    totalDamageDealtToChampions: num(player.totalDamageDealtToChampions),
    item0: num(player.item0),
    item1: num(player.item1),
    item2: num(player.item2),
    item3: num(player.item3),
    item4: num(player.item4),
    item5: num(player.item5),
    item6: num(player.item6),
    summoner1Id: num(player.summoner1Id),
    summoner2Id: num(player.summoner2Id),
    perks: summarisePerks(player.perks),
    placement: num(player.placement),
    playerSubteamId: num(player.playerSubteamId),
  });
}

/**
 * `perks.styles` is an array of two entries — primary and sub — each holding
 * its own selections. All an overview needs from it is the keystone and the two
 * style IDs, which is three numbers rather than the forty-odd lines Riot spends
 * on them. Undefined when the shape is not what we expect: a missing rune row
 * is not worth guessing at.
 */
function summarisePerks(value: unknown): PerksSummary | undefined {
  const styles = obj(value)?.styles;
  if (!Array.isArray(styles)) return undefined;

  const primary = styles.map((s) => obj(s)).find((s) => s?.description === 'primaryStyle');
  const sub = styles.map((s) => obj(s)).find((s) => s?.description === 'subStyle');
  const selections = Array.isArray(primary?.selections) ? primary.selections : [];

  const perks = compact({
    keystone: num(obj(selections[0])?.perk),
    primaryStyle: num(primary?.style),
    subStyle: num(sub?.style),
  });
  return Object.keys(perks).length > 0 ? perks : undefined;
}

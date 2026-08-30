import { Type } from '@sinclair/typebox';
import { ERROR_CODES } from '../errors.js';
import { PLATFORMS, REGIONS } from '../riot/routing.js';

/**
 * §12.3 — closed enums for platform/region, clamped counts, and Riot ID length
 * bounds, all enforced before any upstream call is made.
 */

export const PlatformParam = Type.Unsafe<string>({
  type: 'string',
  enum: [...PLATFORMS],
  description: 'Riot platform host (e.g. euw1, na1, kr)',
});

export const RegionParam = Type.Unsafe<string>({
  type: 'string',
  enum: [...REGIONS],
  description: 'Riot regional routing value',
});

/**
 * Riot ID bounds per §12.3. The maxima are Riot's documented hard caps, so a
 * longer value cannot name a real account and is worth rejecting for free. The
 * minima are only "non-empty": Riot's 3-character floor is an account-creation
 * rule, not an API contract, and legacy and staff accounts sit below it (the
 * 2-character region tag lines handed out during the Riot ID rollout, for one).
 * Rejecting those here would 400 a real player to save a request we can afford.
 * See NinjaGoldfinch/riot-proxy#11.
 */
export const GAME_NAME_MIN = 1;
export const GAME_NAME_MAX = 16;
export const TAG_LINE_MIN = 1;
export const TAG_LINE_MAX = 5;

export const GameNameParam = Type.String({
  minLength: GAME_NAME_MIN,
  maxLength: GAME_NAME_MAX,
  description: 'Riot ID game name (the part before the #)',
});

export const TagLineParam = Type.String({
  minLength: TAG_LINE_MIN,
  maxLength: TAG_LINE_MAX,
  description: 'Riot ID tag line (the part after the #)',
});

/** PUUIDs are 78-character encrypted strings, but length varies by key era. */
export const PuuidParam = Type.String({
  minLength: 60,
  maxLength: 128,
  pattern: '^[A-Za-z0-9_-]+$',
  description: "Encrypted PUUID (valid only for the proxy's current Riot key)",
});

export const MatchIdParam = Type.String({
  minLength: 6,
  maxLength: 40,
  pattern: '^[A-Za-z0-9]+_[0-9]+$',
  description: 'Match ID, e.g. EUW1_7381937461',
});

/** §6.2 — match id list: `count` clamped to 1–100 by Riot's own limit. */
export const MatchIdsQuery = Type.Object({
  start: Type.Optional(Type.Integer({ minimum: 0, maximum: 10_000, default: 0 })),
  count: Type.Optional(Type.Integer({ minimum: 1, maximum: 100, default: 20 })),
  queue: Type.Optional(Type.Integer({ minimum: 0, maximum: 5000 })),
  type: Type.Optional(
    Type.Unsafe<string>({ type: 'string', enum: ['ranked', 'normal', 'tourney', 'tutorial'] }),
  ),
  startTime: Type.Optional(Type.Integer({ minimum: 0 })),
  endTime: Type.Optional(Type.Integer({ minimum: 0 })),
});

export const MasteryQuery = Type.Object({
  top: Type.Optional(Type.Integer({ minimum: 1, maximum: 200 })),
});

export const ErrorResponse = Type.Object(
  {
    error: Type.Object({
      code: Type.Unsafe<string>({ type: 'string', enum: [...ERROR_CODES] }),
      message: Type.String(),
      retryAfter: Type.Optional(Type.Number()),
    }),
  },
  { $id: 'ErrorResponse' },
);

/**
 * Riot payloads pass through unmodified (§6.1), so response bodies are declared
 * as unconstrained — validating Riot's schema here would break the proxy every
 * time Riot adds a field.
 */
export const PassthroughResponse = Type.Unsafe<unknown>({});

export const errorResponses = {
  400: ErrorResponse,
  401: ErrorResponse,
  403: ErrorResponse,
  404: ErrorResponse,
  429: ErrorResponse,
  502: ErrorResponse,
  503: ErrorResponse,
};

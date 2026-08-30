/**
 * Response examples (#63).
 *
 * These live in the docs layer, not on the routes, and deliberately: an example
 * attached here is documentation and nothing else. It is never compiled into a
 * validator, never handed to fast-json-stringify, and cannot change what the
 * service returns. That separation is what makes it safe to describe Riot's
 * bodies without constraining them — §6.1 keeps the `200` schemas empty, and an
 * example that goes stale because Riot added a field is a cosmetic problem
 * where a schema that goes stale is an outage.
 *
 * Every identifier below is fake and visibly so. PUUIDs are the right length
 * and character set but spell out what they are.
 */

const FAKE_PUUID =
  'EXAMPLE-puuid-not-a-real-account-0000000000000000000000000000000000000000000000';

/** Keyed by OpenAPI path, which is how `plugin.ts` looks them up. */
export const RIOT_EXAMPLES: Record<string, unknown> = {
  '/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}': {
    puuid: FAKE_PUUID,
    gameName: 'ExamplePlayer',
    tagLine: 'OCE',
  },
  '/v1/riot/accounts/by-puuid/{region}/{puuid}': {
    puuid: FAKE_PUUID,
    gameName: 'ExamplePlayer',
    tagLine: 'OCE',
  },
  '/v1/lol/summoners/by-puuid/{platform}/{puuid}': {
    puuid: FAKE_PUUID,
    profileIconId: 5789,
    revisionDate: 1_756_001_894_000,
    summonerLevel: 412,
  },
  '/v1/lol/league/entries/by-puuid/{platform}/{puuid}': [
    {
      queueType: 'RANKED_SOLO_5x5',
      tier: 'PLATINUM',
      rank: 'IV',
      leaguePoints: 12,
      wins: 143,
      losses: 131,
      hotStreak: false,
      veteran: false,
      freshBlood: false,
      inactive: false,
    },
  ],
  '/v1/lol/matches/ids/{region}/{puuid}': [
    'OC1_1234567890',
    'OC1_1234567889',
    'OC1_1234567888',
  ],
  '/v1/lol/matches/{region}/{matchId}': {
    metadata: {
      dataVersion: '2',
      matchId: 'OC1_1234567890',
      participants: [FAKE_PUUID, '…nine more…'],
    },
    info: {
      gameCreation: 1_756_000_000_000,
      gameDuration: 1834,
      gameEndTimestamp: 1_756_001_894_000,
      gameMode: 'CLASSIC',
      gameVersion: '15.16.673.9260',
      queueId: 420,
      endOfGameResult: 'GameComplete',
      participants: [
        {
          puuid: FAKE_PUUID,
          championId: 64,
          championName: 'LeeSin',
          teamId: 100,
          teamPosition: 'JUNGLE',
          win: true,
          kills: 8,
          deaths: 3,
          assists: 11,
          '…': 'about 130 more fields per participant, plus a ~100-field `challenges` object',
        },
      ],
      teams: ['…two team objects with bans and objectives…'],
    },
  },
  '/v1/lol/matches/{region}/{matchId}/timeline': {
    metadata: { dataVersion: '2', matchId: 'OC1_1234567890' },
    info: {
      frameInterval: 60_000,
      frames: [
        {
          timestamp: 60_000,
          events: [{ type: 'LEVEL_UP', timestamp: 61_234, participantId: 3, level: 2 }],
          participantFrames: { '1': { '…': 'position, gold, xp, damage stats per minute' } },
        },
      ],
    },
  },
  '/v1/lol/spectator/active/{platform}/{puuid}': {
    gameId: 1_234_567_890,
    gameType: 'MATCHED_GAME',
    gameQueueConfigId: 420,
    gameStartTime: 1_756_000_000_000,
    platformId: 'OC1',
    gameMode: 'CLASSIC',
    participants: [{ puuid: FAKE_PUUID, championId: 64, teamId: 100, spell1Id: 11, spell2Id: 4 }],
  },
  '/v1/lol/mastery/by-puuid/{platform}/{puuid}': [
    {
      puuid: FAKE_PUUID,
      championId: 64,
      championLevel: 7,
      championPoints: 214_882,
      lastPlayTime: 1_756_001_894_000,
      chestGranted: true,
    },
  ],
  '/v1/lol/rotations/{platform}': {
    freeChampionIds: [1, 22, 64, 103, 157],
    freeChampionIdsForNewPlayers: [18, 81, 92, 141],
    maxNewPlayerLevel: 10,
  },
  '/v1/lol/status/{platform}': {
    id: 'OC1',
    name: 'Oceania',
    locales: ['en_AU'],
    maintenances: [],
    incidents: [],
  },
};

/**
 * One worked example per error code. The two throttles are the reason this
 * exists: a 429 and a 503 look alike in a status column and are different
 * problems with different fixes, and until now only prose said so.
 */
export const ERROR_EXAMPLES: Record<string, { summary: string; value: unknown }> = {
  '400': {
    summary: 'Unknown platform or region',
    value: {
      error: {
        code: 'BAD_REGION',
        message: 'params/platform must be equal to one of the allowed values',
      },
    },
  },
  '401': {
    summary: 'Missing or unrecognised key',
    value: { error: { code: 'UNAUTHORIZED', message: 'Missing or invalid consumer key' } },
  },
  '403': {
    summary: 'Key lacks the scope (or the admin IP allowlist rejected it)',
    value: { error: { code: 'FORBIDDEN', message: 'This key does not carry the admin scope' } },
  },
  '404': {
    summary: 'No such resource',
    value: { error: { code: 'NOT_FOUND', message: 'No account for that Riot ID' } },
  },
  '429': {
    summary: 'Your quota — slow down. Sent with Retry-After',
    value: {
      error: {
        code: 'QUOTA_EXCEEDED',
        message: 'Quota of 600/min exceeded',
        retryAfter: 37,
      },
    },
  },
  '502': {
    summary: 'Riot answered, unusably',
    value: { error: { code: 'UPSTREAM_ERROR', message: 'Upstream returned 500' } },
  },
  '503': {
    summary: 'Riot is down, or our limiter is shedding load — affects everyone, not just you',
    value: {
      error: {
        code: 'UPSTREAM_UNAVAILABLE',
        message: 'Rate limit budget exhausted for this scope',
        retryAfter: 4,
      },
    },
  },
};

/**
 * Where the generated snippet is not enough. Both of these encode a thing the
 * schema states but does not teach: that the cooldown is readable without
 * spending one to discover it, and that paging is driven by `hasMore` rather
 * than by guessing when a history ends.
 */
export const CODE_SAMPLES: Record<string, { lang: string; label: string; source: string }[]> = {
  '/v1/players/by-riot-id/{gameName}/{tagLine}/profile': [
    {
      lang: 'JavaScript',
      label: 'Read the refresh cooldown',
      source: `const res = await fetch(
  '/v1/players/by-riot-id/ExamplePlayer/OCE/profile?platform=oc1',
  { headers: { Authorization: 'Bearer ' + KEY } },
);
const profile = await res.json();

// Which parts came back, and how stale each one is — the top-level
// X-Cache-Age is the stalest, which would mislabel the other three.
for (const [part, age] of Object.entries(profile.ageSeconds)) {
  if (age === null) console.warn(part + ' unavailable:', profile.warnings);
  else console.log(part + ' is ' + age + 's old');
}

// Render the Update button's cooldown without spending one to find out.
button.disabled = profile.refreshAvailableIn > 0;
button.title = profile.refreshAvailableIn
  ? 'Try again in ' + profile.refreshAvailableIn + 's'
  : 'Refresh from Riot';`,
    },
  ],
  '/v1/players/{puuid}/matches': [
    {
      lang: 'JavaScript',
      label: 'Page a full history',
      source: `// hasMore says a full page came back, so page on it rather than
// guessing where a history ends. Matches are immutable and archived,
// so every page after the first costs no Riot quota at all.
const all = [];
let start = 0;

for (;;) {
  const res = await fetch(
    \`/v1/players/\${puuid}/matches?platform=oc1&start=\${start}&count=20\`,
    { headers: { Authorization: 'Bearer ' + KEY } },
  );

  if (res.status === 429) {
    // Your own quota, not Riot's. Retry-After is the window.
    await sleep(Number(res.headers.get('Retry-After') ?? 1) * 1000);
    continue;
  }

  const page = await res.json();
  all.push(...page.matches);

  // The first lookup of a player queues their whole history.
  if (page.backfill) console.log('archiving in the background:', page.backfill.jobId);

  if (!page.hasMore) break;
  start += page.count;
}`,
    },
  ],
};

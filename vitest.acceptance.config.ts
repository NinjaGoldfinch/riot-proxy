import { defineConfig } from 'vitest/config';

/**
 * Kept out of `npm test`: these checks hit the real Riot API with a real key,
 * take minutes rather than seconds, and are gated on `ACCEPTANCE_RIOT_ID`.
 */
export default defineConfig({
  test: {
    environment: 'node',
    include: ['acceptance/**/*.test.ts'],
    globalSetup: ['acceptance/helpers/setup.ts'],
    // Phase 2 paces itself against Riot's buckets; Phase 6 waits a poll cycle.
    testTimeout: 15 * 60_000,
    hookTimeout: 2 * 60_000,
    // Shared rate-limit buckets and one archive make parallel phases lie to
    // each other — a backfill running during Phase 2 would blow its 429 budget.
    fileParallelism: false,
    sequence: { concurrent: false },
    reporters: ['verbose'],
  },
});

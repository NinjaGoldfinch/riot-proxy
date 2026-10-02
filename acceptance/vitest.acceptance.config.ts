import { BaseSequencer, type TestSpecification } from 'vitest/node';
import { defineConfig } from 'vitest/config';

/**
 * Phases run in order, 1 → 7, every time. vitest would otherwise reorder files
 * by past duration, and the phases share one server: a crawl that archives
 * everything before Phase 5 leaves it nothing to backfill.
 */
class PhaseOrder extends BaseSequencer {
  override async sort(files: TestSpecification[]): Promise<TestSpecification[]> {
    return [...files].sort((a, b) => a.moduleId.localeCompare(b.moduleId));
  }
}

/**
 * v1's acceptance suite, run against riot-proxy v2 (plan P8-01, ADR-059):
 * `just acceptance` or `cd acceptance && npm test`. Mock mode by default;
 * `ACCEPTANCE_LIVE=1` for v1's live checks against the real Riot API.
 */
export default defineConfig({
  test: {
    environment: 'node',
    include: ['*.test.ts'],
    globalSetup: ['helpers/setup.ts'],
    // Phase 2 paces itself against the buckets; Phase 6 waits a poll cycle.
    testTimeout: 15 * 60_000,
    hookTimeout: 2 * 60_000,
    // Shared rate-limit buckets and one archive make parallel phases lie to
    // each other — a backfill running during Phase 2 would blow its 429 budget.
    fileParallelism: false,
    sequence: { concurrent: false, shuffle: false, sequencer: PhaseOrder },
    reporters: ['verbose'],
  },
});

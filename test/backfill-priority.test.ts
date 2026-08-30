import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import {
  ARCHIVE_PRIORITY,
  BACKFILL_PRIORITY_BASE,
  BACKFILL_PRIORITY_BLOCK,
  MAX_PRIORITY,
  backfillPriority,
} from '../src/jobs/queues.js';

/**
 * Someone who has just been looked up should watch their history fill in from
 * the top, not wait behind a stranger's 2022 season — so the archive queue is
 * ordered by how far back a match sits, and priorities are global rather than
 * per player.
 */
describe('archive queue ordering', () => {
  it('ranks the first block above every later one', () => {
    expect(backfillPriority(0)).toBe(BACKFILL_PRIORITY_BASE);
    expect(backfillPriority(BACKFILL_PRIORITY_BLOCK - 1)).toBe(BACKFILL_PRIORITY_BASE);
    expect(backfillPriority(BACKFILL_PRIORITY_BLOCK)).toBeGreaterThan(backfillPriority(0));
    expect(backfillPriority(900)).toBeGreaterThan(backfillPriority(100));
  });

  it('keeps a freshly finished game ahead of any backfill', () => {
    expect(ARCHIVE_PRIORITY.live).toBeLessThan(backfillPriority(0));
  });

  it('puts one player’s first page ahead of another player’s tenth', () => {
    // The comparison that matters: priority is a single global ordering, so
    // the same numbers decide between players as within one.
    expect(backfillPriority(0)).toBeLessThan(backfillPriority(100));
  });

  it('clamps to the highest priority BullMQ will accept', () => {
    expect(backfillPriority(Number.MAX_SAFE_INTEGER)).toBe(MAX_PRIORITY);
    expect(backfillPriority(-5)).toBe(BACKFILL_PRIORITY_BASE);
  });
});

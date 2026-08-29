import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { JOB, jobKey } from '../src/jobs/queues.js';

/**
 * BullMQ's own validation, transcribed from `Job.create`. A custom id may not
 * be an integer, and may not contain ':' unless it splits into exactly three
 * parts — the shape reserved for repeatable-job keys. Every id we compose has
 * to satisfy this or the enqueue throws at runtime rather than at build time.
 */
function bullmqAccepts(id: string): boolean {
  if (`${parseInt(id, 10)}` === id) return false;
  return !id.includes(':') || id.split(':').length === 3;
}

const PUUID = 'aBcD-1234_efgh';
const MATCH_ID = 'OC1_678901234';

describe('custom job ids (§10)', () => {
  it('agrees with bullmq on the ids that used to throw', () => {
    expect(bullmqAccepts('backfill:PUUID')).toBe(false);
    expect(bullmqAccepts('archive:OC1_1')).toBe(false);
    expect(bullmqAccepts('poll:live:PUUID:1700000000')).toBe(false);
  });

  it('strips the colons our job names carry', () => {
    expect(jobKey(JOB.pollLive, PUUID, 1_700_000_000)).toBe(`poll-live-${PUUID}-1700000000`);
    expect(jobKey('archive', MATCH_ID)).toBe(`archive-${MATCH_ID}`);
  });

  it('produces an acceptable id for every job name and every composed shape', () => {
    for (const name of Object.values(JOB)) {
      expect(bullmqAccepts(jobKey(name, PUUID, 1_700_000_000))).toBe(true);
      expect(bullmqAccepts(jobKey(name, PUUID))).toBe(true);
    }
    expect(bullmqAccepts(jobKey('backfill', PUUID))).toBe(true);
    expect(bullmqAccepts(jobKey('archive', MATCH_ID))).toBe(true);
  });

  it('stays stable for the same inputs, so it still de-duplicates', () => {
    expect(jobKey('archive', MATCH_ID)).toBe(jobKey('archive', MATCH_ID));
  });
});

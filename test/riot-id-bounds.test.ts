import './helpers/env.js';
import { Value } from '@sinclair/typebox/value';
import { describe, expect, it } from 'vitest';
import {
  GAME_NAME_MAX,
  GAME_NAME_MIN,
  GameNameParam,
  TAG_LINE_MAX,
  TAG_LINE_MIN,
  TagLineParam,
} from '../src/routes/schemas.js';

/**
 * §12.3 — the Riot ID bounds exist to reject what Riot cannot possibly answer,
 * not to re-implement Riot's account-creation rules. Anything at or below the
 * documented maxima has to reach upstream; see NinjaGoldfinch/riot-proxy#11.
 */
describe('Riot ID length bounds (§12.3)', () => {
  it('only rejects an empty game name at the bottom', () => {
    expect(GAME_NAME_MIN).toBe(1);
    expect(Value.Check(GameNameParam, '')).toBe(false);
    expect(Value.Check(GameNameParam, 'a')).toBe(true);
    expect(Value.Check(GameNameParam, 'ab')).toBe(true);
  });

  it('only rejects an empty tag line at the bottom', () => {
    expect(TAG_LINE_MIN).toBe(1);
    expect(Value.Check(TagLineParam, '')).toBe(false);
    expect(Value.Check(TagLineParam, '1')).toBe(true);
  });

  it('accepts the 2-character region tag lines from the Riot ID rollout', () => {
    for (const tag of ['EU', 'NA', 'AP', 'KR']) {
      expect(Value.Check(TagLineParam, tag)).toBe(true);
    }
  });

  it("still rejects anything past Riot's documented maxima", () => {
    expect(GAME_NAME_MAX).toBe(16);
    expect(TAG_LINE_MAX).toBe(5);
    expect(Value.Check(GameNameParam, 'x'.repeat(GAME_NAME_MAX))).toBe(true);
    expect(Value.Check(GameNameParam, 'x'.repeat(GAME_NAME_MAX + 1))).toBe(false);
    expect(Value.Check(TagLineParam, 'x'.repeat(TAG_LINE_MAX))).toBe(true);
    expect(Value.Check(TagLineParam, 'x'.repeat(TAG_LINE_MAX + 1))).toBe(false);
  });
});

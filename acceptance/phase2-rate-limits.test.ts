import { beforeAll, describe, expect, it } from 'vitest';
import { acceptance, cfg } from './helpers/env.js';
import { counter, get, histogramP95, metrics, passthrough, pool } from './helpers/harness.js';

/**
 * Phase 2 — hammer one method and prove the limiter, not Riot, is what paced
 * us. The pass condition is zero *accountable* 429s: `application` and
 * `method` types mean our own accounting drifted. A `service` 429 is Riot's
 * edge shedding load and is explicitly not our fault (§9.4).
 */
const enabled = acceptance.enabled;

interface LimitUsage {
  scope: string;
  usage: { window: string; used: number; limit: number }[];
  frozenMs: number;
}

/**
 * Bucket math: pushing N requests through a window of `limit` per `seconds`
 * cannot force the last one to wait longer than one window per full bucket
 * ahead of it. p95 must sit inside that, or the limiter is over-waiting.
 */
function explainableWaitSeconds(usage: LimitUsage['usage'], requests: number): number | undefined {
  if (usage.length === 0) return undefined;
  let bound = 0;
  for (const window of usage) {
    const seconds = Number(window.window.split(':')[1]);
    if (!Number.isFinite(seconds) || window.limit <= 0) continue;
    bound = Math.max(bound, Math.floor((requests - 1) / window.limit) * seconds);
  }
  return bound;
}

let puuid = '';

describe.skipIf(!enabled)('Phase 2 — rate limiter under load', () => {
  beforeAll(async () => {
    const { gameName, tagLine, region } = cfg();
    const account = await get<{ puuid: string }>(
      `/v1/riot/accounts/by-riot-id/${region}/${encodeURIComponent(gameName)}/${encodeURIComponent(tagLine)}`,
    );
    puuid = account.body.puuid;
  });

  it('takes no accountable 429s across a burst on one method', async () => {
    const { region, phase2Requests } = cfg();
    const idsPath = (start: number) =>
      `/lol/match/v5/matches/by-puuid/${puuid}/ids?start=${start}&count=1`;

    // One warm-up so the limiter has learned the real windows from Riot's
    // headers; before that it is running on conservative defaults (§9.1).
    await passthrough(region, idsPath(0), 'match.idsByPuuid');

    const before = await metrics();
    const started = performance.now();

    // Distinct `start` values so each request is its own cache key and cannot
    // be collapsed by single-flight (§8.4) — otherwise this measures nothing.
    const responses = await pool(phase2Requests, 10, (i) =>
      passthrough(region, idsPath(i), 'match.idsByPuuid', { noCache: 'true' }),
    );

    const elapsedMs = performance.now() - started;
    const after = await metrics();
    const limits = await get<LimitUsage>(`/v1/admin/limits/${region}`);

    // Which window overflowed is the whole diagnosis: a window still far from
    // its limit rules itself out, leaving the one that actually bound us.
    const windows = limits.body.usage.map((w) => `${w.window} at ${w.used}/${w.limit}`).join(', ');

    for (const type of ['application', 'method'] as const) {
      const seen =
        counter(after, 'proxy_rl_429_total', { type }) -
        counter(before, 'proxy_rl_429_total', { type });
      expect(
        seen,
        `${seen} accountable ${type} 429s over ${phase2Requests} requests in ` +
          `${(elapsedMs / 1000).toFixed(1)}s. Windows after the burst: ${windows}. ` +
          `A window well under its limit did not cause this — look at the tightest one, ` +
          `and at whether a burst straddled its boundary.`,
      ).toBe(0);
    }

    // Anything the proxy shed itself must be a well-formed RATE_LIMITED
    // envelope — never a leaked upstream 429 and never a 5xx.
    const byStatus = new Map<number, number>();
    for (const res of responses) byStatus.set(res.status, (byStatus.get(res.status) ?? 0) + 1);
    const served = byStatus.get(200) ?? 0;
    const shed = byStatus.get(429) ?? 0;

    process.stdout.write(
      `  phase 2: ${phase2Requests} requests in ${(elapsedMs / 1000).toFixed(1)}s — ` +
        `${served} served, ${shed} shed by the client wait budget\n`,
    );

    expect(served + shed, `unexpected statuses: ${JSON.stringify([...byStatus])}`).toBe(
      phase2Requests,
    );
    expect(served, 'every request was shed — is CLIENT_WAIT_BUDGET_MS too low?').toBeGreaterThan(0);

    expect(limits.body.frozenMs, 'the scope was frozen, so Riot did the pacing').toBe(0);

    const { p95, count } = histogramP95(before, after, 'proxy_rl_wait_seconds', {
      region,
      priority: 'interactive',
    });
    const bound = explainableWaitSeconds(limits.body.usage, phase2Requests);

    if (bound === undefined) {
      process.stdout.write('  phase 2: limiter has not learned any window yet, skipping p95\n');
      return;
    }
    process.stdout.write(
      `  phase 2: p95 wait ${p95}s over ${count} dispatches, bucket bound ${bound}s\n`,
    );
    // A histogram p95 lands on a bucket edge, so allow the next bucket up.
    expect(p95, 'p95 wait is not explainable by the learned buckets').toBeLessThanOrEqual(
      bound + 1,
    );
  });
});

/**
 * Whether a missing Redis or Postgres is allowed to pass for a green run.
 *
 * Every integration test in this suite probes for its services and skips when
 * they are not reachable. That is the right default: the unit suite runs on a
 * laptop with nothing up, and a developer who has not started Docker gets a
 * useful run rather than a wall of connection errors. The cost is that
 * "260 passed" and "260 passed, most of them nothing" print identically, and
 * the ambiguity points the wrong way — green reads as covered.
 *
 * `REQUIRE_SERVICES=1` turns the skip into a failure. CI provides both
 * services, so it sets the flag and the assertions there mean what they look
 * like they mean; nothing changes locally.
 *
 * Nothing under `src/` is imported here. Several suites pin an environment
 * variable at module scope and then import `src/config.js` dynamically, and a
 * static import from a helper would evaluate config first and read the value
 * they were about to set.
 */
export const SERVICES_REQUIRED = process.env['REQUIRE_SERVICES'] === '1';

/**
 * A probe has to be bounded. The shared Redis client retries forever, which is
 * right for a service that will come back and wrong here: an unbounded `ping()`
 * against nothing turns "skip this file" into a hook timeout twenty seconds
 * later, which is neither the skip nor the clear failure anyone wanted.
 */
const PROBE_TIMEOUT_MS = 3000;

/**
 * Run a suite's own reachability check, and decide what its answer means.
 * `label` names the suite in the failure, because the thing worth knowing is
 * which coverage would otherwise have gone quiet.
 */
export async function probeServices(
  label: string,
  probe: () => Promise<boolean>,
): Promise<boolean> {
  let available: boolean;
  try {
    available = await withTimeout(probe());
  } catch {
    available = false;
  }
  return requireServices(available, label);
}

/** For suites that do their own probing inline — same verdict, no probe. */
export function requireServices(available: boolean, label: string): boolean {
  if (!available && SERVICES_REQUIRED) {
    throw new Error(
      `REQUIRE_SERVICES=1, but '${label}' cannot reach the services it needs. ` +
        'Every assertion in that file would have skipped silently. ' +
        'Start Redis and Postgres (docker compose up -d), or unset REQUIRE_SERVICES.',
    );
  }
  return available;
}

function withTimeout<T>(work: Promise<T>): Promise<T> {
  return Promise.race([
    work,
    new Promise<never>((_, reject) => {
      setTimeout(() => reject(new Error('service probe timed out')), PROBE_TIMEOUT_MS).unref();
    }),
  ]);
}

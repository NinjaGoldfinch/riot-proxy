import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { CONNECTIONS, PINNED } from './env.js';

/**
 * The reset tooling is only meaningful as a process: its guards are `process
 * .exitCode` and stderr, and `main()` runs at import, so there is nothing to
 * call. These tests therefore run the real entrypoints and assert on what a
 * developer would actually see.
 */
const execFileAsync = promisify(execFile);

export const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url));
const TSX = fileURLToPath(new URL('../../node_modules/.bin/tsx', import.meta.url));

export interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

/**
 * A child gets the same pinned environment the suite runs under, for the same
 * reason: `src/config.ts` calls `loadDotenv()` at import, and dotenv leaves an
 * already-set variable alone — so passing every `EnvSchema` key explicitly is
 * what stops a developer's `.env` from deciding what these assertions mean.
 *
 * Only PATH and HOME come from the parent, and PATH is overridable so a test
 * can put a shim ahead of a real binary.
 */
export function childEnv(overrides: Record<string, string> = {}): NodeJS.ProcessEnv {
  const base: Record<string, string> = {
    PATH: process.env['PATH'] ?? '',
    HOME: process.env['HOME'] ?? '',
  };
  for (const key of Object.keys(CONNECTIONS)) base[key] = process.env[key] ?? '';
  return { ...base, ...PINNED, ...overrides };
}

async function capture(file: string, args: string[], env: NodeJS.ProcessEnv): Promise<CliResult> {
  try {
    const { stdout, stderr } = await execFileAsync(file, args, {
      cwd: REPO_ROOT,
      env,
      timeout: 60_000,
    });
    return { code: 0, stdout, stderr };
  } catch (err) {
    const e = err as { code?: number | string; stdout?: string; stderr?: string };
    // A non-numeric `code` is a spawn failure (ENOENT and friends), not an exit
    // status — surfacing it as an exit code would make a missing binary look
    // like a guard that fired.
    if (typeof e.code !== 'number') throw err;
    return { code: e.code, stdout: e.stdout ?? '', stderr: e.stderr ?? '' };
  }
}

/** Run one of the `src/cli/*.ts` entrypoints through tsx, as npm does. */
export function runCli(
  script: string,
  args: string[] = [],
  overrides: Record<string, string> = {},
): Promise<CliResult> {
  return capture(TSX, [`src/cli/${script}.ts`, ...args], childEnv(overrides));
}

/** Run a script under `scripts/`. */
export function runScript(
  name: string,
  args: string[] = [],
  overrides: Record<string, string> = {},
  env?: NodeJS.ProcessEnv,
): Promise<CliResult> {
  return capture('bash', [`scripts/${name}`, ...args], env ?? childEnv(overrides));
}

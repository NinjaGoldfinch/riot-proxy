import './helpers/env.js';
import { chmodSync, mkdtempSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { childEnv, runScript } from './helpers/cli.js';

/**
 * `scripts/reset.sh` destroys Docker volumes, `dist/`, `data/` and
 * `node_modules`, so the suite tests exactly the paths that refuse to get
 * there — the guards are the only part of it that is safe to execute.
 *
 * Every run goes out with a fake `docker` ahead of the real one on PATH. It
 * records that it was called and then **fails**, so a guard that regresses
 * stops at `docker compose down` (the first destructive line) instead of
 * reaching the `rm -rf` two lines later. The sentinel is the assertion; the
 * non-zero exit is the seatbelt.
 */
let shimDir: string;
let sentinel: string;

beforeAll(() => {
  shimDir = mkdtempSync(join(tmpdir(), 'reset-shim-'));
  sentinel = join(shimDir, 'docker-was-called');
  const shim = join(shimDir, 'docker');
  writeFileSync(shim, `#!/bin/sh\necho "$@" >> "${sentinel}"\nexit 1\n`);
  chmodSync(shim, 0o755);
});

afterAll(() => {
  rmSync(shimDir, { recursive: true, force: true });
});

function guardedEnv(overrides: Record<string, string> = {}): NodeJS.ProcessEnv {
  const env = childEnv(overrides);
  env['PATH'] = `${shimDir}:${env['PATH'] ?? ''}`;
  return env;
}

function run(args: string[], overrides: Record<string, string> = {}) {
  return runScript('reset.sh', args, {}, guardedEnv(overrides));
}

describe('scripts/reset.sh', () => {
  it('prints usage and exits clean for --help', async () => {
    const { code, stdout } = await run(['--help']);
    expect(code).toBe(0);
    expect(stdout).toContain('Usage: npm run reset:all');
    expect(stdout).toContain('--keep-deps');
    expect(existsSync(sentinel)).toBe(false);
  });

  it('rejects an unknown option with exit 2 rather than guessing', async () => {
    const { code, stderr } = await run(['--delete-everything']);
    expect(code).toBe(2);
    expect(stderr).toContain('Unknown option');
    expect(existsSync(sentinel)).toBe(false);
  });

  /** The one that matters: this script is a developer tool, not an operation. */
  it('refuses to run under NODE_ENV=production', async () => {
    const { code, stderr } = await run(['--yes'], { NODE_ENV: 'production' });
    expect(code).toBe(1);
    expect(stderr).toContain('NODE_ENV=production');
    expect(existsSync(sentinel)).toBe(false);
  });

  /**
   * CI has no terminal. A prompt that reads EOF as "yes" would make this
   * script destructive by default in exactly the place nobody is watching.
   */
  it('aborts rather than assuming yes when stdin is not a terminal', async () => {
    const { code, stderr } = await run([]);
    expect(code).toBe(1);
    expect(stderr).toContain('Not a terminal');
    expect(existsSync(sentinel)).toBe(false);
  });

  it('states what it will destroy before asking', async () => {
    const { stdout } = await run([]);
    expect(stdout).toContain('This will destroy:');
    expect(stdout).toContain('It will not touch .env.');
  });
});

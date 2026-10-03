import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { acceptance, MOCK_ADMIN_KEY } from './env.js';
import { startMockRiot, type MockRiot } from './mock-riot.js';

/**
 * Acceptance checks are black-box: they talk to a running `riot-proxy serve`
 * over HTTP and WebSocket, never to anything in-process (v1).
 *
 * Mock mode (the default) starts a scripted Riot and a fresh `serve` pointed at
 * it, on a scratch DATA_DIR, with a known admin key — so a run starts from an
 * empty archive every time and needs no orchestration in CI.
 *
 * Live mode uses a server already listening at ACCEPTANCE_BASE_URL, or starts
 * one from the repo's `.env` (v1 did the same with `npm run dev`).
 */
let child: ChildProcess | undefined;
let mock: MockRiot | undefined;
let dataDir: string | undefined;

const repo = new URL('../../', import.meta.url).pathname;
const binary = process.env['RIOT_PROXY_BIN'] ?? join(repo, 'target/debug/riot-proxy');

async function isUp(baseUrl: string): Promise<boolean> {
  try {
    const res = await fetch(new URL('/readyz', baseUrl), { signal: AbortSignal.timeout(2000) });
    return res.ok;
  } catch {
    return false;
  }
}

function start(env: Record<string, string>, cwd: string): ChildProcess {
  const proc = spawn(binary, ['serve'], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
  // Surface crashes; otherwise a dead server just looks like a timeout.
  proc.stderr?.on('data', (chunk: Buffer) => process.stderr.write(`[serve] ${chunk}`));
  proc.on('exit', (code) => {
    if (code !== 0 && code !== null) process.stderr.write(`[serve] exited with ${code}\n`);
  });
  return proc;
}

async function ready(baseUrl: string): Promise<void> {
  const deadline = Date.now() + 60_000;
  while (!(await isUp(baseUrl))) {
    if (child?.exitCode !== null && child?.exitCode !== undefined) {
      throw new Error(`serve exited (${child.exitCode}) before becoming ready`);
    }
    if (Date.now() >= deadline) throw new Error(`serve did not become ready at ${baseUrl} within 60s`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

export async function setup(): Promise<void> {
  if (!acceptance.enabled) {
    process.stdout.write(`acceptance: SKIPPED — ${acceptance.reason}\n`);
    return;
  }
  const { baseUrl, mode, port, mockPort, platform } = acceptance;

  if (mode === 'live') {
    if (await isUp(baseUrl)) {
      process.stdout.write(`acceptance: live, using the server already running at ${baseUrl}\n`);
      return;
    }
    process.stdout.write(`acceptance: live, starting serve from ${repo}.env\n`);
    child = start({ ...(process.env as Record<string, string>), PORT: String(port) }, repo);
    await ready(baseUrl);
    return;
  }

  if (await isUp(baseUrl)) {
    throw new Error(`something is already listening at ${baseUrl}; mock mode needs its own serve`);
  }
  mock = await startMockRiot(mockPort);
  dataDir = mkdtempSync(join(tmpdir(), 'riot-proxy-acceptance-'));
  // A clean environment: nothing of the developer's, and in particular not the
  // real key from `.env` — serve runs in the scratch dir, which has none.
  child = start(
    {
      PATH: process.env['PATH'] ?? '',
      RIOT_API_KEY: 'RGAPI-acceptance-mock-key',
      DATA_DIR: dataDir,
      HOST: '127.0.0.1',
      PORT: String(port),
      RIOT_BASE_URL: mock.url,
      DDRAGON_BASE_URL: mock.url,
      BOOTSTRAP_ADMIN_KEY: MOCK_ADMIN_KEY,
      DEFAULT_PLATFORM: platform,
      LADDER_PLATFORMS: platform,
      LADDER_QUEUES: 'RANKED_SOLO_5x5',
      LADDER_TIER_FLOOR: 'MASTER',
      AGGREGATE_MIN_GAMES: '0',
      TRACK_POLL_LIVE_S: '10',
      TRACK_POLL_RANK_S: '30',
      TRACK_POLL_MATCH_S: '30',
      // A spectator answer is cached 30 s by default: a game the mock ends
      // would wait that long to be noticed. v1's override knob, shortened.
      CACHE_TTL_OVERRIDES: 'spectator=5',
      LOG_LEVEL: process.env['ACCEPTANCE_LOG_LEVEL'] ?? 'warn',
      LOG_FORMAT: 'json',
    },
    dataDir,
  );
  await ready(baseUrl);
  process.stdout.write(`acceptance: mock Riot at ${mock.url}, serve at ${baseUrl} (${binary})\n`);
}

export async function teardown(): Promise<void> {
  if (child) {
    child.kill('SIGTERM');
    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        child?.kill('SIGKILL');
        resolve();
      }, 15_000);
      child?.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }
  await mock?.close();
  if (dataDir) rmSync(dataDir, { recursive: true, force: true });
}

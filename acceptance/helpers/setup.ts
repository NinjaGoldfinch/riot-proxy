import { spawn, type ChildProcess } from 'node:child_process';
import { acceptance } from './env.js';

/**
 * Acceptance checks are black-box: they talk to a running api and worker over
 * HTTP, not to an in-process Fastify instance, because Phases 5 and 6 are only
 * meaningful if the worker is actually draining queues.
 *
 * If something is already listening (the usual `npm run dev` pair) we use it
 * and leave it alone. Otherwise we start both and tear them down afterwards,
 * so CI needs no extra orchestration.
 */
const children: ChildProcess[] = [];

async function isUp(baseUrl: string): Promise<boolean> {
  try {
    const res = await fetch(new URL('/readyz', baseUrl), {
      signal: AbortSignal.timeout(2000),
    });
    return res.ok;
  } catch {
    return false;
  }
}

function start(entry: string): ChildProcess {
  const child = spawn('npx', ['tsx', entry], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, LOG_LEVEL: process.env['ACCEPTANCE_LOG_LEVEL'] ?? 'warn' },
  });
  // Surface upstream auth failures and crashes; otherwise a spawned process
  // dying just looks like an unexplained timeout.
  child.stderr?.on('data', (chunk: Buffer) => process.stderr.write(`[${entry}] ${chunk}`));
  child.on('exit', (code) => {
    if (code !== 0 && code !== null) process.stderr.write(`[${entry}] exited with ${code}\n`);
  });
  children.push(child);
  return child;
}

export async function setup(): Promise<void> {
  if (!acceptance.enabled) {
    process.stdout.write(`acceptance: SKIPPED — ${acceptance.reason}\n`);
    return;
  }
  const { baseUrl } = acceptance;

  if (await isUp(baseUrl)) {
    process.stdout.write(`acceptance: using the server already running at ${baseUrl}\n`);
    return;
  }

  process.stdout.write('acceptance: starting api + worker\n');
  start('src/index.ts');
  start('src/worker.ts');

  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (await isUp(baseUrl)) return;
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`api did not become ready at ${baseUrl} within 60s`);
}

export async function teardown(): Promise<void> {
  for (const child of children) child.kill('SIGTERM');
  if (children.length === 0) return;
  await new Promise((r) => setTimeout(r, 1000));
  for (const child of children) if (!child.killed) child.kill('SIGKILL');
}

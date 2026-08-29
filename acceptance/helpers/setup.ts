import { spawn, type ChildProcess } from 'node:child_process';
import { Queue } from 'bullmq';
import { Redis } from 'ioredis';
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

/**
 * Mirrors src/jobs/queues.ts rather than importing it. This file is
 * deliberately black-box, and importing the queue module would build the app's
 * Redis singleton and all five queues inside the test runner.
 */
const CANARY_QUEUE = 'maintenance';
const CANARY_JOB = 'maintenance';

/** Long enough for a live worker to claim a job, short enough not to drag. */
const PROBE_MS = 3000;

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

/**
 * `/readyz` proves an api is listening; it says nothing about whether anything
 * is draining queues, and `npm run dev` on its own satisfies it. Phases 5 and 6
 * are meaningless without a consumer — 6 waits out its timeout, and 5 watches
 * queues that never move.
 *
 * BullMQ's getWorkersCount() would answer this from CLIENT LIST, but it trusts
 * the connection rather than the process: a worker killed outright keeps its
 * Redis client registered until the server reaps it, and the naming it relies
 * on needs CLIENT SETNAME, which not every hosted Redis allows. Prove it
 * functionally instead — enqueue a real maintenance pass (idempotent, no
 * upstream calls, it only sweeps leaked single-flight locks) and see whether
 * anything picks it up.
 */
async function workerAlive(redis: Redis, timeoutMs: number): Promise<boolean> {
  const queue = new Queue(CANARY_QUEUE, { connection: redis });
  const job = await queue.add(CANARY_JOB, {}, { jobId: `probe-${Date.now()}`, attempts: 1 });
  try {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      // Claimed at all is the signal we want; a pass that errored still proves
      // a consumer exists, which is the only question being asked here.
      const state = await job.getState();
      if (state === 'active' || state === 'completed' || state === 'failed') return true;
      if (Date.now() >= deadline) return false;
      await new Promise((r) => setTimeout(r, 250));
    }
  } finally {
    await job.remove().catch(() => {});
    await queue.close();
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
  const { baseUrl, redisUrl } = acceptance;
  const redis = new Redis(redisUrl, { maxRetriesPerRequest: null });

  try {
    const apiUp = await isUp(baseUrl);
    const workerUp = await workerAlive(redis, PROBE_MS);

    if (apiUp && workerUp) {
      process.stdout.write(`acceptance: using the api and worker already running at ${baseUrl}\n`);
      return;
    }

    // Start only what is missing: someone running `npm run dev` without
    // `npm run dev:worker` should get a worker, not a second api.
    const missing = [!apiUp && 'api', !workerUp && 'worker'].filter(Boolean).join(' + ');
    process.stdout.write(`acceptance: starting ${missing}\n`);
    if (!apiUp) start('src/index.ts');
    if (!workerUp) start('src/worker.ts');

    const deadline = Date.now() + 60_000;
    while (!(await isUp(baseUrl))) {
      if (Date.now() >= deadline)
        throw new Error(`api did not become ready at ${baseUrl} within 60s`);
      await new Promise((r) => setTimeout(r, 1000));
    }
    if (!(await workerAlive(redis, Math.max(PROBE_MS, deadline - Date.now())))) {
      throw new Error('no worker is draining the queues — phases 5 and 6 cannot run');
    }
  } finally {
    await redis.quit();
  }
}

export async function teardown(): Promise<void> {
  for (const child of children) child.kill('SIGTERM');
  if (children.length === 0) return;
  await new Promise((r) => setTimeout(r, 1000));
  for (const child of children) if (!child.killed) child.kill('SIGKILL');
}

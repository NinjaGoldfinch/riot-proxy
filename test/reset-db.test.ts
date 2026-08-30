import './helpers/env.js';
import postgres from 'postgres';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { runCli } from './helpers/cli.js';

/**
 * `reset:db` truncates every table it finds, so running it against the
 * database the developer is working in would empty their match archive on
 * `npm test`. Each case runs against a **throwaway database** created here and
 * dropped afterwards.
 *
 * The schema below is deliberately synthetic rather than the migrated one: what
 * needs proving is that the CLI truncates what `pg_tables` reports, skips
 * `consumers` on request, quotes identifiers it does not control, and stays
 * inside the `public` schema. A hand-built schema states each of those in one
 * table apiece, and does not go stale when a migration lands.
 */
const TEST_DB = 'riot_proxy_reset_test';

let adminUrl: string;
let testUrl: string;
let sql: postgres.Sql | undefined;
let available = false;

beforeAll(async () => {
  const url = new URL(process.env['DATABASE_URL'] ?? '');
  testUrl = new URL(url.toString()).toString().replace(/\/[^/]*$/, `/${TEST_DB}`);
  url.pathname = '/postgres';
  adminUrl = url.toString();

  const admin = postgres(adminUrl, { max: 1, connect_timeout: 5, onnotice: () => {} });
  try {
    await admin`select 1`;
    // `drop … with (force)` detaches anyone still connected from a previous run.
    await admin.unsafe(`drop database if exists ${TEST_DB} with (force)`);
    await admin.unsafe(`create database ${TEST_DB}`);
    available = true;
  } catch {
    available = false;
  } finally {
    await admin.end({ timeout: 5 });
  }

  if (!available) return;
  sql = postgres(testUrl, { max: 2, connect_timeout: 5, onnotice: () => {} });
  await sql.unsafe(`
    create table consumers (id serial primary key, name text not null);
    create table players (puuid text primary key);
    create table "MixedCase" (id serial primary key);
    create schema drizzle;
    create table drizzle.__drizzle_migrations (id serial primary key, hash text);
  `);
});

afterAll(async () => {
  if (sql) await sql.end({ timeout: 5 });
  if (!available) return;
  const admin = postgres(adminUrl, { max: 1, connect_timeout: 5, onnotice: () => {} });
  try {
    await admin.unsafe(`drop database if exists ${TEST_DB} with (force)`);
  } finally {
    await admin.end({ timeout: 5 });
  }
});

beforeEach(async () => {
  if (!available || !sql) return;
  await sql.unsafe(`
    truncate consumers, players, "MixedCase" restart identity;
    insert into consumers (name) values ('keep-me');
    insert into players (puuid) values ('puuid-1');
    insert into "MixedCase" default values;
    delete from drizzle.__drizzle_migrations;
    insert into drizzle.__drizzle_migrations (hash) values ('0000_initial');
  `);
});

async function counts(): Promise<Record<string, number>> {
  const s = sql!;
  const [c] = await s`select count(*)::int as n from consumers`;
  const [p] = await s`select count(*)::int as n from players`;
  const [m] = await s`select count(*)::int as n from "MixedCase"`;
  const [d] = await s`select count(*)::int as n from drizzle.__drizzle_migrations`;
  return { consumers: c!['n'], players: p!['n'], mixed: m!['n'], migrations: d!['n'] };
}

const run = (args: string[] = [], overrides: Record<string, string> = {}) =>
  runCli('reset-db', args, { DATABASE_URL: testUrl, ...overrides });

describe.runIf(process.env['SKIP_DB_TESTS'] !== '1')('reset:db', () => {
  it('empties every table in the public schema', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run();
    expect(code).toBe(0);
    expect(stdout).toContain('Truncated 3 table(s)');
    const after = await counts();
    expect(after.consumers).toBe(0);
    expect(after.players).toBe(0);
    // A table named in mixed case only truncates if the CLI quotes it.
    expect(after.mixed).toBe(0);
  });

  /**
   * The docstring promises no re-migration afterwards. That holds only because
   * drizzle keeps its bookkeeping in its own schema and the CLI filters on
   * `schemaname = 'public'` — worth pinning, since it is one WHERE clause away
   * from silently becoming a rebuild.
   */
  it('leaves the migration bookkeeping alone, so no re-migration is needed', async ({ skip }) => {
    if (!available) return skip();
    await run();
    expect((await counts()).migrations).toBe(1);
  });

  it('keeps the schema itself, not just the rows', async ({ skip }) => {
    if (!available) return skip();
    await run();
    const tables = await sql!`
      select tablename from pg_tables where schemaname = 'public' order by tablename
    `;
    expect(tables.map((t) => t['tablename'])).toEqual(['MixedCase', 'consumers', 'players']);
  });

  it('--keep-consumers spares minted API keys', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run(['--keep-consumers']);
    expect(code).toBe(0);
    expect(stdout).toContain('Kept: consumers');
    const after = await counts();
    expect(after.consumers).toBe(1);
    expect(after.players).toBe(0);
  });

  it('refuses under NODE_ENV=production and truncates nothing', async ({ skip }) => {
    if (!available) return skip();
    const { code, stderr } = await run([], { NODE_ENV: 'production' });
    expect(code).toBe(1);
    expect(stderr).toContain('Refusing to reset the database');
    const after = await counts();
    expect(after.consumers).toBe(1);
    expect(after.players).toBe(1);
  });

  it('--help explains itself without touching anything', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run(['--help']);
    expect(code).toBe(0);
    expect(stdout).toContain('Usage: npm run reset:db');
    expect((await counts()).players).toBe(1);
  });
});

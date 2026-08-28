import { drizzle } from 'drizzle-orm/postgres-js';
import postgres from 'postgres';
import { config } from '../config.js';
import * as schema from './schema.js';

/**
 * A modest pool: the api process is I/O bound on Riot and Redis, not Postgres,
 * and the archive writes go through the worker.
 */
export const sql = postgres(config.DATABASE_URL, {
  max: config.isTest ? 2 : 10,
  idle_timeout: 30,
  connect_timeout: 10,
  onnotice: () => {},
});

export const db = drizzle(sql, { schema });

export type Database = typeof db;
export { schema };

export async function closeDb(): Promise<void> {
  await sql.end({ timeout: 5 });
}

export async function pingDb(): Promise<boolean> {
  try {
    await sql`select 1`;
    return true;
  } catch {
    return false;
  }
}

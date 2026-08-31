import { createHash, randomBytes } from 'node:crypto';
import { and, eq, isNull } from 'drizzle-orm';
import { KEY_PREFIX } from '../keys.js';
import { DEFAULT_QUOTA_PER_MIN } from '../quotas.js';
import { db } from './index.js';
import { consumers, type Consumer } from './schema.js';

export function generateKey(): string {
  // 24 random bytes → 32 base64url characters, no padding.
  return KEY_PREFIX + randomBytes(24).toString('base64url');
}

export function hashKey(key: string): string {
  return createHash('sha256').update(key).digest('hex');
}

export interface CreateConsumerInput {
  name: string;
  scopes?: string[];
  quotaPerMin?: number;
  /** Supply to import an existing key (bootstrap); otherwise one is generated. */
  key?: string;
  ifNotExists?: boolean;
}

export interface CreatedConsumer {
  id: string;
  name: string;
  scopes: string[];
  quotaPerMin: number;
  /** Plaintext key — shown exactly once, never retrievable again. */
  key: string;
}

export async function createConsumer(
  input: CreateConsumerInput,
): Promise<CreatedConsumer | undefined> {
  const key = input.key ?? generateKey();
  const keyHash = hashKey(key);

  if (input.ifNotExists) {
    const existing = await db.select().from(consumers).where(eq(consumers.keyHash, keyHash));
    if (existing.length > 0) return undefined;
  }

  const [row] = await db
    .insert(consumers)
    .values({
      name: input.name,
      keyHash,
      scopes: input.scopes ?? ['read'],
      quotaPerMin: input.quotaPerMin ?? DEFAULT_QUOTA_PER_MIN,
    })
    .returning();

  if (!row) throw new Error('failed to create consumer');
  return { id: row.id, name: row.name, scopes: row.scopes, quotaPerMin: row.quotaPerMin, key };
}

export async function findConsumerByHash(keyHash: string): Promise<Consumer | undefined> {
  const rows = await db
    .select()
    .from(consumers)
    .where(and(eq(consumers.keyHash, keyHash), isNull(consumers.disabledAt)))
    .limit(1);
  return rows[0];
}

/**
 * A consumer by id *and* key hash, disabled ones included.
 *
 * `revoke-cache` names a consumer in the path and a key in the body, and used
 * to act on the body alone — so any uuid revoked any hash and the route's shape
 * promised a relationship it did not enforce. Matching both in the query keeps
 * the comparison where a hash lookup already lives, and `disabledAt` comes back
 * with the row so the caller can still be told whether the key is live.
 */
export async function findConsumerByIdAndHash(
  id: string,
  keyHash: string,
): Promise<Consumer | undefined> {
  const rows = await db
    .select()
    .from(consumers)
    .where(and(eq(consumers.id, id), eq(consumers.keyHash, keyHash)))
    .limit(1);
  return rows[0];
}

export async function listConsumers(): Promise<Omit<Consumer, 'keyHash'>[]> {
  const rows = await db.select().from(consumers);
  return rows.map(({ keyHash: _keyHash, ...rest }) => rest);
}

/** Soft delete: the hash stays so the key can never be silently reissued. */
export async function disableConsumer(id: string): Promise<boolean> {
  const rows = await db
    .update(consumers)
    .set({ disabledAt: new Date() })
    .where(and(eq(consumers.id, id), isNull(consumers.disabledAt)))
    .returning({ id: consumers.id });
  return rows.length > 0;
}

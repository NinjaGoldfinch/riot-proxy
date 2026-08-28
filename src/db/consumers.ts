import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';
import { and, eq, isNull } from 'drizzle-orm';
import { db } from './index.js';
import { consumers, type Consumer } from './schema.js';

/** §12.1 — `rpx_<32 chars>`; only the sha256 is ever persisted. */
export const KEY_PREFIX = 'rpx_';

export function generateKey(): string {
  // 24 random bytes → 32 base64url characters, no padding.
  return KEY_PREFIX + randomBytes(24).toString('base64url');
}

export function hashKey(key: string): string {
  return createHash('sha256').update(key).digest('hex');
}

/** Constant-time compare so a hash lookup cannot be timed. */
export function hashesMatch(a: string, b: string): boolean {
  const bufA = Buffer.from(a, 'utf8');
  const bufB = Buffer.from(b, 'utf8');
  if (bufA.length !== bufB.length) return false;
  return timingSafeEqual(bufA, bufB);
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
      quotaPerMin: input.quotaPerMin ?? 600,
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

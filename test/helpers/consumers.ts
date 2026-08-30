import { inArray } from 'drizzle-orm';
import {
  createConsumer,
  type CreateConsumerInput,
  type CreatedConsumer,
} from '../../src/db/consumers.js';
import { db } from '../../src/db/index.js';
import { consumers } from '../../src/db/schema.js';

/**
 * Consumers minted by the suite are hard-deleted afterwards.
 *
 * `disableConsumer` is a deliberate soft delete (§12.1: the hash stays so a key
 * can never be silently reissued). That is the right guarantee for a real
 * issued key and the wrong one for a fixture — without a hard delete the table
 * grows by several live, usable keys on every run, which buries the real
 * consumers in `GET /v1/admin/consumers` and leaves working credentials behind
 * on any database shared between runs.
 */
const created: string[] = [];

/** Shared prefix so a stray row is recognisable and a manual sweep is trivial. */
export const TEST_CONSUMER_PREFIX = 'vitest';

export function testConsumerName(label: string): string {
  return `${TEST_CONSUMER_PREFIX}-${label}-${Date.now()}`;
}

export async function createTestConsumer(
  input: CreateConsumerInput,
): Promise<CreatedConsumer | undefined> {
  const consumer = await createConsumer(input);
  if (consumer) created.push(consumer.id);
  return consumer;
}

/** For consumers minted through the admin API, which this module never sees. */
export function trackTestConsumer(id: string): void {
  created.push(id);
}

export async function removeTestConsumers(): Promise<void> {
  if (created.length === 0) return;
  const ids = created.splice(0, created.length);
  await db.delete(consumers).where(inArray(consumers.id, ids));
}

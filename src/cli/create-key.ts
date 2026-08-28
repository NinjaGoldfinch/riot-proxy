import { parseArgs } from 'node:util';
import { createConsumer } from '../db/consumers.js';
import { closeDb } from '../db/index.js';

/**
 * Phase 4 — `npm run key:create -- --name web`. Prints the plaintext key once;
 * only its sha256 is stored (§12.1).
 */
async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      name: { type: 'string', short: 'n' },
      scopes: { type: 'string', short: 's', default: 'read' },
      quota: { type: 'string', short: 'q', default: '600' },
    },
  });

  if (!values.name) {
    console.error('Usage: npm run key:create -- --name <name> [--scopes read,admin] [--quota 600]');
    process.exitCode = 1;
    return;
  }

  const scopes = values.scopes
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
  const quota = Number(values.quota);
  if (!Number.isInteger(quota) || quota < 1) {
    console.error(`Invalid --quota '${values.quota}'`);
    process.exitCode = 1;
    return;
  }

  const created = await createConsumer({ name: values.name, scopes, quotaPerMin: quota });
  if (!created) {
    console.error('Failed to create consumer');
    process.exitCode = 1;
    return;
  }

  console.log('');
  console.log('  Consumer created');
  console.log('  ────────────────');
  console.log(`  id        ${created.id}`);
  console.log(`  name      ${created.name}`);
  console.log(`  scopes    ${created.scopes.join(', ')}`);
  console.log(`  quota     ${created.quotaPerMin}/min`);
  console.log('');
  console.log(`  API KEY   ${created.key}`);
  console.log('');
  console.log('  This key is shown once and cannot be recovered. Store it now.');
  console.log('');
}

main()
  .catch((err: unknown) => {
    console.error(err);
    process.exitCode = 1;
  })
  .finally(() => closeDb());

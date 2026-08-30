import { parseArgs } from 'node:util';
import { config } from '../config.js';
import { closeDb, sql } from '../db/index.js';

/**
 * Local teardown — `npm run reset:db`. Empties the tables but leaves the schema
 * and the migration bookkeeping alone, so this is a data reset, not a rebuild:
 * no re-migration needed afterwards. For a rebuild use `scripts/reset.sh`,
 * which drops the volume entirely.
 */

const USAGE = `Usage: npm run reset:db [-- --keep-consumers]

  (default)          truncate every table in the public schema
  --keep-consumers   leave the consumers table alone, so minted API keys survive
  --force            allow the reset while NODE_ENV=production`;

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      'keep-consumers': { type: 'boolean', default: false },
      force: { type: 'boolean', default: false },
      help: { type: 'boolean', short: 'h', default: false },
    },
  });

  if (values.help) {
    console.log(USAGE);
    return;
  }

  if (config.isProduction && !values.force) {
    console.error(
      'Refusing to reset the database with NODE_ENV=production. Pass --force if you mean it.',
    );
    process.exitCode = 1;
    return;
  }

  // Read the table list from the database rather than from the schema module,
  // so a table added without touching this file is still truncated.
  const rows = await sql<{ tablename: string }[]>`
    select tablename from pg_tables where schemaname = 'public' order by tablename
  `;

  const tables = rows
    .map((r) => r.tablename)
    .filter((name) => !(values['keep-consumers'] && name === 'consumers'));

  if (tables.length === 0) {
    console.log(
      'Nothing to truncate — the public schema has no tables. Run `npm run migrate` first.',
    );
    return;
  }

  // Identifiers come straight from pg_tables, but quote them anyway: a table
  // named in mixed case or with a reserved word breaks the statement otherwise.
  const list = tables.map((name) => `"${name.replaceAll('"', '""')}"`).join(', ');
  await sql.unsafe(`truncate table ${list} restart identity cascade`);

  console.log(`Truncated ${tables.length} table(s): ${tables.join(', ')}`);
  if (values['keep-consumers']) console.log('Kept: consumers (your API keys still work).');
}

main()
  .catch((err: unknown) => {
    console.error(err);
    process.exitCode = 1;
  })
  .finally(() => closeDb());

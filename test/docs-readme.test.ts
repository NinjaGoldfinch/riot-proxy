import './helpers/env.js';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { __test } from '../src/config.js';
import { ENDPOINTS, METHOD_IDS, type MethodId } from '../src/riot/endpoints.js';

/**
 * The README, checked against the code it describes.
 *
 * `test/docs-*.test.ts` and the CI drift check cover the *generated* reference:
 * a route that changes its schema cannot ship an `openapi.json` that disagrees
 * with it. The README is the other half of the contract and had nothing —
 * every number, default, path and command in it was hand-written and free to
 * rot. This file asserts the mechanical half of it: the claims that have a
 * source of truth in the repo.
 *
 * It deliberately asserts nothing about the prose. "Bulk stands aside while
 * interactive requests are queued" is a claim about behaviour, and the place to
 * assert it is the limiter suite, not a regex over a paragraph.
 *
 * Nothing here needs Redis or Postgres, so unlike every integration suite this
 * one cannot skip itself — which is the point. Only `src/config.ts` and
 * `src/riot/endpoints.ts` are imported, neither of which opens a connection.
 */

const ROOT = fileURLToPath(new URL('../', import.meta.url));
const read = (rel: string): string => readFileSync(join(ROOT, rel), 'utf8');
const exists = (rel: string): boolean => {
  try {
    statSync(join(ROOT, rel));
    return true;
  } catch {
    return false;
  }
};

const README = read('README.md');
/** Prettier wraps prose at 80 columns, so a sentence is not a line. */
const PROSE = README.replace(/\s+/g, ' ');
const PACKAGE = JSON.parse(read('package.json')) as {
  scripts: Record<string, string>;
  engines: { node: string };
};
const properties = __test.EnvSchema.properties as Record<string, { default?: unknown }>;

/** Everything between a `##`/`###` heading and the next one at or above it. */
function section(title: string): string {
  const lines = README.split('\n');
  const start = lines.findIndex((l) => /^#{2,3} /.test(l) && l.replace(/^#+ /, '') === title);
  expect(start, `README has no "${title}" heading`).toBeGreaterThan(-1);
  const level = /^#+/.exec(lines[start]!)![0].length;
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((l) => /^#+ /.test(l) && /^#+/.exec(l)![0].length <= level);
  return (end === -1 ? rest : rest.slice(0, end)).join('\n');
}

/** A markdown table as cells, header and separator rows dropped. */
function rows(text: string): string[][] {
  return text
    .split('\n')
    .filter((l) => l.trim().startsWith('|'))
    .map((l) =>
      l
        .trim()
        .replace(/^\|/, '')
        .replace(/\|$/, '')
        .split('|')
        .map((c) => c.trim()),
    )
    .filter((cells) => !cells.every((c) => c === '' || /^:?-+:?$/.test(c)));
}

const ticks = (cell: string): string[] => [...cell.matchAll(/`([^`]+)`/g)].map((m) => m[1]!);

/**
 * The env-var column abbreviates a family: ``TRACK_POLL_LIVE_S` / `_RANK_S``.
 * A leading-underscore token supplies the last N segments of the first name.
 */
function envNames(cell: string): string[] {
  const tokens = ticks(cell);
  const first = tokens[0] ?? '';
  return tokens.map((token) => {
    if (!token.startsWith('_')) return token;
    const supplied = token.slice(1).split('_').length;
    return `${first.split('_').slice(0, -supplied).join('_')}${token}`;
  });
}

const CONFIG_ROWS = rows(section('Configuration')).filter((r) => envNames(r[0] ?? '').length > 0);

describe('the configuration table', () => {
  it('lists every setting the service reads, and nothing it does not', () => {
    const documented = CONFIG_ROWS.flatMap((r) => envNames(r[0]!));
    // Set equality both ways: a setting added to `EnvSchema` and left out of
    // the table is undocumented, and a row for a setting that no longer exists
    // is worse — it reads as configurable and is ignored.
    expect([...documented].sort()).toEqual(Object.keys(properties).sort());
  });

  it('documents the default the schema actually applies', () => {
    for (const row of CONFIG_ROWS) {
      const names = envNames(row[0]!);
      const cell = row[1] ?? '';
      const values = ticks(cell);

      names.forEach((name, i) => {
        const fallback = properties[name]?.default;
        const documented = names.length === 1 ? values[0] : values[i];

        // No schema default means required (RIOT_API_KEY) or derived from
        // something else (DEV_UI follows NODE_ENV). Either way the cell must
        // say so rather than quoting a value that does not exist.
        if (fallback === undefined) {
          expect(
            cell === '—' || /follows/.test(cell),
            `${name} has no schema default, so the table must not quote one (found "${cell}")`,
          ).toBe(true);
          return;
        }

        // An em dash is how the table writes "empty by default".
        if (documented === undefined) {
          expect(String(fallback), `${name} is documented as unset`).toBe('');
          return;
        }

        // One default is quoted in truncated form; the prefix is what matters.
        if (documented.includes('…')) {
          expect(String(fallback)).toContain(documented.split('…')[0]!.trim());
          return;
        }

        if (typeof fallback === 'number') {
          expect(Number(documented), `${name} default`).toBe(fallback);
          return;
        }
        expect(documented, `${name} default`).toBe(String(fallback));
      });
    }
  });

  it("keeps the ceiling's arithmetic honest", () => {
    // The paragraph explains 0.80 twice over — as a percentage, and as a
    // deviation from the spec's opening 75 %. Both are derived numbers written
    // out by hand, so both drift the moment the default moves.
    const ceiling = properties['BULK_USAGE_CEILING']!.default as number;
    const full = /a bucket is (\d+)% full/.exec(PROSE);
    expect(full, 'the README no longer explains the ceiling as a percentage').not.toBeNull();
    expect(Number(full![1]), 'the percentage the README explains').toBe(ceiling * 100);

    const deviation = /the extra (\d+)% is a deliberate deviation/.exec(PROSE);
    expect(deviation, 'the deviation from the spec is no longer stated').not.toBeNull();
    expect(Number(deviation![1]), 'the stated deviation from the spec').toBe(ceiling * 100 - 75);
  });
});

/**
 * A `KEY=…` line, commented or not. A commented entry documents an opt-in —
 * `# DEV_UI=true` says "this exists and is off" — so it counts as documented.
 */
const ENV_DOCUMENTED = new Set(
  [...read('.env.example').matchAll(/^#?\s*([A-Z][A-Z0-9_]*)=/gm)].map((m) => m[1]!),
);

/**
 * `.env.example` is the first file a new developer opens and the only
 * documentation most settings have. It is not generated, on purpose: the
 * comments explaining each knob are TS block comments in `src/config.ts` that
 * TypeBox cannot see, and two of the values differ from the schema default
 * deliberately. So the key set is asserted and nothing about the values is.
 */
describe('.env.example', () => {
  it('documents every setting the schema accepts', () => {
    // The risk is quiet and one-directional: a setting added to `EnvSchema` and
    // not here is invisible — the service starts fine on the default and nobody
    // learns the knob exists. That is how `DOCS_UI` would have gone unmentioned.
    for (const key of Object.keys(properties)) {
      expect(ENV_DOCUMENTED, `${key} is in EnvSchema but not in .env.example`).toContain(key);
    }
  });

  it('does not advertise a setting the schema has dropped', () => {
    // Worse than a missing one: it reads as supported and does nothing.
    for (const key of ENV_DOCUMENTED) {
      expect(properties, `.env.example sets ${key}, which EnvSchema ignores`).toHaveProperty(key);
    }
  });
});

describe('numbers quoted beside the setting they come from', () => {
  it('matches the schema every time', () => {
    // `TRACK_POLL_LIVE_S` (60 s), `BULK_USAGE_CEILING` (default 75 %) — the
    // shape the README uses when it names a setting and its value in one
    // breath. Cheap to write, and the first thing to go stale.
    const quoted = [
      ...README.matchAll(/`([A-Z][A-Z0-9_]+)`[^.\n]{0,40}?\((?:default )?([\d.]+) ?(s|%)\)/g),
    ];
    expect(quoted.length, 'nothing matched — has the README changed shape?').toBeGreaterThan(3);

    for (const [, name, value, unit] of quoted) {
      const fallback = properties[name!]?.default;
      expect(fallback, `${name} is quoted in the README but is not a setting`).toBeDefined();
      const expected = unit === '%' ? Number(value) / 100 : Number(value);
      expect(expected, `${name} quoted as ${value} ${unit}`).toBe(fallback);
    }
  });
});

/**
 * Which method ids each row of the caching table stands for. The map is the
 * hand-written part; the numbers come from `ENDPOINTS`, and the coverage
 * assertion below means a new endpoint cannot be added without a row here and
 * a row in the README.
 */
const CACHE_ROWS: Record<string, MethodId[]> = {
  'Match / timeline': ['match.byId', 'match.timeline'],
  'Match ID list': ['match.idsByPuuid'],
  'Account (either direction)': ['account.byRiotId', 'account.byPuuid'],
  Summoner: ['summoner.byPuuid'],
  'League entries': ['league.entriesByPuuid'],
  'Ladder (apex + paged)': [
    'league.challenger',
    'league.grandmaster',
    'league.master',
    'league.entriesByTier',
  ],
  Spectator: ['spectator.activeGame'],
  'Champion mastery': ['mastery.byPuuid', 'mastery.topByPuuid'],
  'Champion rotations': ['platform.championRotations'],
  'Platform status': ['status.platformData'],
};

/** "24 h" -> 86 400, "120 s" -> 120, "forever (…)" -> Infinity. */
function seconds(cell: string): number {
  if (/forever/i.test(cell)) return Infinity;
  const match = /(\d+)\s*(s|h)\b/.exec(cell);
  expect(match, `no TTL in "${cell}"`).not.toBeNull();
  return Number(match![1]) * (match![2] === 'h' ? 3600 : 1);
}

describe('the caching table', () => {
  const table = rows(section('Caching')).filter((r) => CACHE_ROWS[r[0] ?? '']);

  it('covers every endpoint the proxy caches', () => {
    expect(Object.values(CACHE_ROWS).flat().sort()).toEqual([...METHOD_IDS].sort());
    expect(table.map((r) => r[0]).sort()).toEqual(Object.keys(CACHE_ROWS).sort());
  });

  it('quotes the TTL each endpoint actually has', () => {
    for (const [label, cell] of table) {
      const documented = seconds(cell!);
      for (const id of CACHE_ROWS[label!]!) {
        const spec = ENDPOINTS.find((e) => e.id === id)!;
        expect(documented, `${label} (${id})`).toBe(spec.ttlSeconds);
      }
    }
  });

  it('quotes the negative TTL beside the endpoint that needs one', () => {
    // Spectator 404s are the only negative cache the table mentions, and the
    // number is `NEG_TTL_SECONDS` rather than a property of the endpoint.
    const spectator = table.find(([label]) => label === 'Spectator')![1]!;
    const negative = /\(\+\s*(\d+)\s*s negative\)/.exec(spectator);
    expect(negative, 'the spectator row no longer states its negative TTL').not.toBeNull();
    expect(Number(negative![1])).toBe(properties['NEG_TTL_SECONDS']!.default);
  });
});

describe('the commands the README tells you to run', () => {
  it('all exist in package.json', () => {
    const scripts = [...README.matchAll(/npm run ([\w:]+)/g)].map((m) => m[1]!);
    expect(scripts.length).toBeGreaterThan(5);
    for (const script of new Set(scripts)) {
      expect(PACKAGE.scripts, `npm run ${script} is documented but not a script`).toHaveProperty(
        script,
      );
    }
  });

  it('agrees with .nvmrc and engines about the Node major', () => {
    const claimed = /Node (\d+) is required/.exec(PROSE);
    expect(claimed, 'the README no longer states a Node version').not.toBeNull();
    expect(read('.nvmrc').trim()).toBe(claimed![1]);
    expect(PACKAGE.engines.node).toContain(`>=${claimed![1]}`);
  });
});

/** `test/reset-{cache,db,script}.test.ts` -> the three paths it stands for. */
function expandBraces(path: string): string[] {
  const match = /^(.*)\{([^}]+)\}(.*)$/.exec(path);
  if (!match) return [path];
  return match[2]!.split(',').map((option) => `${match[1]}${option}${match[3]}`);
}

const MARKDOWN = [
  'README.md',
  'TODO.md',
  ...readdirSync(join(ROOT, 'docs'))
    .filter((f) => f.endsWith('.md'))
    .map((f) => `docs/${f}`),
];

/**
 * GitHub's own rules: lowercase, drop everything that is not a word character,
 * a space or a hyphen, then turn each remaining space into a hyphen. Each
 * space, not each run — `1. Overview & Goals` anchors as `1-overview--goals`,
 * and a slug that collapses the pair silently stops matching real links.
 */
const slug = (heading: string): string =>
  heading
    .toLowerCase()
    .trim()
    .replace(/[^\w -]/g, '')
    .replace(/ /g, '-');

describe('every path the docs point at', () => {
  it('resolves — links, in all four documents', () => {
    for (const doc of MARKDOWN) {
      const text = read(doc);
      const targets = [...text.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)].map((m) => m[1]!);
      const headings = new Set([...text.matchAll(/^#+ (.+)$/gm)].map((m) => slug(m[1]!)));

      for (const target of targets) {
        if (/^(https?:|mailto:)/.test(target)) continue;
        if (target.startsWith('#')) {
          expect(headings, `${doc}: no heading for ${target}`).toContain(target.slice(1));
          continue;
        }
        const [path] = target.split('#');
        // Relative to the document, not the repo root: docs/ links to its
        // sibling as `riot-proxy-spec.md`.
        const from = doc.includes('/') ? `${doc.slice(0, doc.lastIndexOf('/'))}/` : '';
        expect(exists(`${from}${path}`) || exists(path!), `${doc}: ${target} does not exist`).toBe(
          true,
        );
      }
    }
  });

  it('resolves — the files the README names in prose', () => {
    // Only paths with a directory in them, plus bare test filenames. A bare
    // name is otherwise ambiguous: `riot-proxy-spec_1.md` is the spec's own
    // title for itself, not a file in this repo.
    const named = [...README.matchAll(/`([\w./{},-]+\.(?:ts|json|ya?ml|html|sh|md))`/g)]
      .map((m) => m[1]!)
      .flatMap(expandBraces);
    expect(named.length).toBeGreaterThan(5);

    for (const path of new Set(named)) {
      // `/openapi.yaml` is a route this service serves, not a file on disk.
      if (path.startsWith('/')) continue;
      if (path.includes('/')) {
        expect(exists(path), `README names ${path}, which does not exist`).toBe(true);
      } else if (path.endsWith('.test.ts')) {
        expect(
          exists(`test/${path}`) || exists(`acceptance/${path}`),
          `README names ${path}, which is in neither test/ nor acceptance/`,
        ).toBe(true);
      }
    }
  });
});

/**
 * Only the tree column names files. The comment column beside it is prose —
 * "get/set/neg, soft+hard TTL" is not a directory — so both patterns anchor on
 * the box-drawing glyph (or the start of a line, for the three roots).
 */
const TREE_ENTRY_TS = /(?:^|[\u2502\u251c\u2514\u2500] )\s*([a-z][\w-]*\.ts)/gm;
const TREE_ENTRY_DIR = /(?:^|[\u2502\u251c\u2514\u2500] )\s*([a-z][\w-]*)\//gm;

describe('the layout diagram', () => {
  const block = /```\n([\s\S]*?)```/.exec(section('Layout'))![1]!;
  const srcDirs = readdirSync(join(ROOT, 'src')).filter((f) =>
    statSync(join(ROOT, 'src', f)).isDirectory(),
  );

  const walk = (dir: string): string[] =>
    readdirSync(join(ROOT, dir)).flatMap((entry) => {
      const rel = `${dir}/${entry}`;
      return statSync(join(ROOT, rel)).isDirectory() ? walk(rel) : [entry];
    });

  it('names only modules that exist', () => {
    const modules = new Set(walk('src'));
    for (const [, file] of block.matchAll(TREE_ENTRY_TS)) {
      expect(modules, `the diagram shows src/…/${file}, which does not exist`).toContain(file);
    }
  });

  it('shows every directory under src/, and no others', () => {
    const drawn = new Set([...block.matchAll(TREE_ENTRY_DIR)].map((m) => m[1]!));
    for (const dir of srcDirs) {
      expect(drawn, `src/${dir}/ is missing from the diagram`).toContain(dir);
    }
    for (const dir of drawn) {
      expect(
        srcDirs.includes(dir) || exists(dir),
        `the diagram shows ${dir}/, which is neither in src/ nor at the root`,
      ).toBe(true);
    }
  });
});

describe('the acceptance table', () => {
  const documented = rows(section('Acceptance checks'))
    .flatMap((r) => ticks(r[0] ?? ''))
    .filter((t) => t.startsWith('ACCEPTANCE_'));

  it('documents every ACCEPTANCE_ variable the suite reads', () => {
    const walk = (dir: string): string[] =>
      readdirSync(join(ROOT, dir)).flatMap((entry) => {
        const rel = `${dir}/${entry}`;
        return statSync(join(ROOT, rel)).isDirectory() ? walk(rel) : [read(rel)];
      });
    const used = new Set(
      walk('acceptance').flatMap((source) =>
        [...source.matchAll(/ACCEPTANCE_[A-Z0-9_]+/g)].map((m) => m[0]),
      ),
    );
    // Both ways again: an undocumented knob is one nobody turns, and a
    // documented one the suite has stopped reading is worse.
    expect([...documented].sort()).toEqual([...used].sort());
  });
});

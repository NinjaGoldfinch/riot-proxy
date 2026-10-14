#!/usr/bin/env python3
"""A synthetic analytics database and the PERF series' read timings (ADR-166,
ADR-167, ADR-168). Not run by CI; run by hand to re-measure.

    python3 scripts/perf-synthetic.py /tmp/perf/s.db

PERF_SQLITE=pysqlite3 runs it on pysqlite3-binary's SQLite (3.51, about the
version rusqlite bundles) instead of Python's own.

Builds a database from src/db/migrations up to V0024 (before PERF-02), fills
the champion tables with seeded random rows (4 platforms, 2 queues, 6 tiers,
8 patches, 171 champions, 1-5 roles each: 375,608 champion_stats rows), then:

1. times the stats and patch reads the site's slice and one platform and
   patch run, as PERF-01 writes them, against champion_stats;
2. applies V0025 and times it, with the pages it adds and frees;
3. times the same reads against champion_stats_totals;
4. times the rebuild's new INSERT for one ladder, every patch.

Each read runs with and without ANALYZE statistics. "cold" is a fresh
connection's first read (empty SQLite page cache; the OS cache is warm);
"warm" is the mean of three more on that connection. A read is what one
Db::read does: the slice's domain, then the SELECT.
"""

import glob
import importlib
import os
import random
import re
import shutil
import sys
import time

sqlite3 = importlib.import_module(os.environ.get("PERF_SQLITE", "sqlite3"))

MIGRATIONS = os.path.join(os.path.dirname(__file__), "..", "src", "db", "migrations")
PATCH_DESC = (
    "CAST(substr(patch, 1, instr(patch, '.') - 1) AS INTEGER) DESC, "
    "CAST(substr(patch, instr(patch, '.') + 1) AS INTEGER) DESC"
)
KS, QUEUE = "k1", "RANKED_SOLO_5x5"


def migrations(upto):
    files = glob.glob(os.path.join(MIGRATIONS, "V*.sql"))
    version = lambda p: int(re.search(r"V(\d+)__", p).group(1))
    return [f for f in sorted(files, key=version) if version(f) <= upto]


def build(path):
    """ADR-166's synthetic copy at V0024, seed 1."""
    if os.path.exists(path):
        os.remove(path)
    c = sqlite3.connect(path)
    for f in migrations(24):
        c.executescript(open(f).read())
    random.seed(1)
    plats = ["oc1", "kr", "euw1", "na1"]
    queues = [QUEUE, "RANKED_FLEX_SR"]
    tiers = ["UNKNOWN", "DIAMOND", "MASTER", "GRANDMASTER", "CHALLENGER", "EMERALD"]
    patches = [f"25.{i}" for i in range(14, 22)]
    roles = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"]
    now = 1760000000000
    st, bn, bt, sl, mt = [], [], [], [], []
    for p in plats:
        for q in queues:
            for pa in patches:
                mt.append((KS, p, q, pa, 0, 5000, now))
                for ch in range(1, 172):
                    bt.append((KS, p, q, pa, ch, 0, random.randint(0, 900), now))
                for t in tiers:
                    sl.append((KS, p, q, t, pa, 0, 3000, now))
                    for ch in range(1, 172):
                        bn.append((KS, p, q, t, pa, ch, 0, random.randint(0, 300), now))
                        for r in random.sample(roles, random.choice([1, 2, 2, 3, 5])):
                            for rem in [0, 1] if random.random() < 0.1 else [0]:
                                for sd in ["blue", "red"]:
                                    g = random.randint(1, 200)
                                    st.append((KS, p, q, t, pa, ch, r, rem, sd, g, g // 2, g, g, g * 5, g * 4,
                                               g * 7, g * 180, g * 11000, g * 20000, g * 30, g * 1800, now))
    c.executemany("INSERT INTO champion_stats VALUES (%s)" % ",".join("?" * 22), st)
    c.executemany("INSERT INTO champion_bans VALUES (?,?,?,?,?,?,?,?,?)", bn)
    c.executemany("INSERT INTO champion_ban_totals VALUES (?,?,?,?,?,?,?,?)", bt)
    c.executemany("INSERT INTO analytics_slices VALUES (?,?,?,?,?,?,?,?)", sl)
    c.executemany("INSERT INTO analytics_match_totals VALUES (?,?,?,?,?,?,?)", mt)
    c.commit()
    c.close()


def domain(c, platform):
    """archive::analytics::domain."""
    sql = "SELECT DISTINCT platform, tier, patch FROM analytics_slices WHERE key_scope = ? AND queue = ?"
    args = [KS, QUEUE]
    if platform:
        sql += " AND platform = ?"
        args.append(platform)
    rows = c.execute(sql, args).fetchall()
    return [sorted({r[i] for r in rows}) for i in range(3)]


def where(c, platform, patch, tier_key):
    """archive::analytics::Filter for a stats read with no tier, role, side or champion."""
    plats, tiers, patches = domain(c, platform)
    sql, args = ["key_scope = ?"], [KS]

    def one_of(col, v, all_):
        if v:
            sql.append(f"{col} = ?")
            args.append(v)
        else:
            sql.append(f"{col} IN ({', '.join('?' * len(all_))})")
            args.extend(all_)

    one_of("platform", platform, plats)
    sql.append("queue = ?")
    args.append(QUEUE)
    if tier_key:
        one_of("tier", None, tiers)
    one_of("patch", patch, patches)
    sql.append("remake = 0")
    return " AND ".join(sql), args


COLUMNS = ["games", "wins", "matches_picked", "stated_games", "kills", "deaths", "assists", "cs", "gold",
           "damage", "vision", "duration_s"]


def stat_rows(c, table, platform, patch):
    """stat_rows with no tier: PERF-01's flat GROUP BY over champion_stats, or
    PERF-02's, which sums per platform and patch first, over the totals."""
    w, args = where(c, platform, patch, table == "champion_stats")
    if table == "champion_stats":
        source = f"{table} WHERE {w}"
    else:
        partial = ", ".join(f"sum({x}) AS {x}" for x in COLUMNS) + ", max(computed_at) AS computed_at"
        source = f"(SELECT platform, patch, champion_id, {partial} FROM {table} WHERE {w} GROUP BY platform, patch, champion_id)"
    sums = ", ".join(f"sum({x})" for x in COLUMNS) + ", max(computed_at)"
    return c.execute(
        f"""SELECT champion_id, NULL, {sums} FROM {source}
             GROUP BY champion_id HAVING sum(games) >= ? ORDER BY sum(games) DESC, champion_id LIMIT ?""",
        args + [0, 500],
    ).fetchall()


def patch_rows(c, table, platform, patch):
    w, args = where(c, platform, None, table == "champion_stats")
    return c.execute(
        f"SELECT patch, sum(games), max(computed_at) FROM {table} WHERE {w} GROUP BY patch ORDER BY {PATCH_DESC}",
        args,
    ).fetchall()


SLICES = [("site slice", None, None), ("oc1, 25.20", "oc1", "25.20")]
READS = [("stat_rows", stat_rows), ("patches", patch_rows)]


def timed(f):
    t = time.perf_counter()
    out = f()
    return (time.perf_counter() - t) * 1000, out


def bench(path, table):
    results = {}
    for name, read in READS:
        for label, platform, patch in SLICES:
            c = sqlite3.connect(path)
            cold, rows = timed(lambda: read(c, table, platform, patch))
            warm = sum(timed(lambda: read(c, table, platform, patch))[0] for _ in range(3)) / 3
            c.close()
            results[(name, label)] = (cold, warm, rows)
    return results


def sizes(path):
    c = sqlite3.connect(path)
    if not c.execute("SELECT count(*) FROM pragma_module_list WHERE name = 'dbstat'").fetchone()[0]:
        return {"dbstat": None}  # not compiled in (pysqlite3-binary)
    out = dict(c.execute(
        "SELECT name, sum(pgsize) FROM dbstat WHERE name IN "
        "('champion_stats', 'champion_stats_slice', 'champion_stats_totals') GROUP BY name"
    ).fetchall())
    out["freelist"] = c.execute("PRAGMA freelist_count").fetchone()[0] * c.execute("PRAGMA page_size").fetchone()[0]
    c.close()
    return out


def mib(n):
    return "n/a" if n is None else f"{n / 1048576:.1f} MiB"


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/perf/s.db"
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    print("SQLite", sqlite3.sqlite_version)
    build(path)
    stats_path = path + ".analyze"
    shutil.copyfile(path, stats_path)
    c = sqlite3.connect(stats_path)
    c.execute("ANALYZE")
    c.commit()
    c.close()
    c = sqlite3.connect(path)
    print("champion_stats rows", c.execute("SELECT count(*) FROM champion_stats").fetchone()[0])
    c.close()
    print("before", {k: mib(v) for k, v in sizes(path).items()})

    before = {s: bench(p, "champion_stats") for s, p in [("no stats", path), ("ANALYZE", stats_path)]}

    v25 = open(migrations(25)[-1]).read()
    for label, p in [("no stats", path), ("ANALYZE", stats_path)]:
        c = sqlite3.connect(p)
        ms, _ = timed(lambda: c.executescript("BEGIN;" + v25 + "COMMIT;"))
        n = c.execute("SELECT count(*) FROM champion_stats_totals").fetchone()[0]
        c.close()
        print(f"V0025 ({label}): {ms:.0f} ms, {n} champion_stats_totals rows")
    print("after", {k: mib(v) for k, v in sizes(path).items()})
    c = sqlite3.connect(stats_path)
    c.execute("ANALYZE champion_stats_totals")
    c.commit()
    c.close()

    after = {s: bench(p, "champion_stats_totals") for s, p in [("no stats", path), ("ANALYZE", stats_path)]}

    print(f"\n{'read':10} {'slice':11} {'stats':9} {'before cold/warm':>20} {'after cold/warm':>20}  same")
    for s in before:
        for (name, label), (bc, bw, brows) in before[s].items():
            ac, aw, arows = after[s][(name, label)]
            print(f"{name:10} {label:11} {s:9} {bc:9.1f} /{bw:7.1f} ms {ac:9.1f} /{aw:7.1f} ms  {brows == arows}")

    # The rebuild's new statement, for one ladder and every patch.
    c = sqlite3.connect(path)
    c.execute("BEGIN")
    c.execute("DELETE FROM champion_stats_totals WHERE key_scope = ? AND platform = 'oc1' AND queue = ?", [KS, QUEUE])
    ms, _ = timed(lambda: c.execute(
        """INSERT INTO champion_stats_totals (key_scope, platform, queue, patch, champion_id, role, remake, side,
             games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
             duration_s, computed_at)
           SELECT key_scope, platform, queue, patch, champion_id, role, remake, side,
             sum(games), sum(wins), sum(matches_picked), sum(stated_games), sum(kills), sum(deaths), sum(assists),
             sum(cs), sum(gold), sum(damage), sum(vision), sum(duration_s), ?4
             FROM champion_stats
            WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
            GROUP BY patch, champion_id, role, remake, side""",
        [KS, "oc1", QUEUE, 1]))
    c.rollback()
    c.close()
    print(f"\nrebuild insert, oc1 {QUEUE}, every patch: {ms:.0f} ms")


if __name__ == "__main__":
    main()

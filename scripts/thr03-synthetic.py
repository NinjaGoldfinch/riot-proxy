#!/usr/bin/env python3
"""THR-03's synthetic archive and its delta prototype (ADR-182; plan
docs/thr/THR-03-plan.md §1, §16). Not run by CI; run by hand to re-measure.

The rebuild here is a port of src/archive/analytics.rs's statements (at
32c4672) and the delta a prototype of src/archive/analytics/delta.rs; the
binary is the authority, this is for measuring shapes and sizes quickly.

    python3 scripts/thr03-synthetic.py gen  DB N            # DB from src/db/migrations V0001-V0028, N matches
    python3 scripts/thr03-synthetic.py migrate DB           # V0029's DDL (prototype), timed
    python3 scripts/thr03-synthetic.py pending DB           # every match uncounted (bits 0)
    python3 scripts/thr03-synthetic.py rebuild DB           # today's rebuild, a transaction a step, timed
    python3 scripts/thr03-synthetic.py rebuild-stamp DB     # the same, stamping each step's matches
    python3 scripts/thr03-synthetic.py rebuild-steps DB a,b # only those steps
    python3 scripts/thr03-synthetic.py delta DB BATCH       # count every pending match, BATCH a transaction
    python3 scripts/thr03-synthetic.py deltas DB BATCH      # the same, every transaction's time listed
    python3 scripts/thr03-synthetic.py arrive DB N SEED     # N more matches (a crawl's arrivals), pending
    python3 scripts/thr03-synthetic.py late DB PCT          # late-stamp PCT % of UNKNOWN facts to MASTER
    python3 scripts/thr03-synthetic.py diff A B             # every analytics table, computed_at left out

THR_KS sets the facts' key scope (d63166ad is the scope of the all-zero test
key, for booting the binary on the file). CKPT sets wal_autocheckpoint and
TEMPMEM=1 temp_store=MEMORY, as the server's writer has.
"""
import glob, json, os, random, re, sqlite3, statistics, sys, time

MIG = os.path.join(os.path.dirname(__file__), "..", "src", "db", "migrations")
KS, PLAT, QUEUE, QID = os.environ.get("THR_KS", "k1"), "oc1", "RANKED_SOLO_5x5", 420
TIERS = ["IRON", "BRONZE", "SILVER", "GOLD", "PLATINUM", "EMERALD", "DIAMOND", "MASTER", "GRANDMASTER", "CHALLENGER"]

def migrations(upto):
    fs = glob.glob(os.path.join(MIG, "V*.sql"))
    v = lambda p: int(re.search(r"V(\d+)__", p).group(1))
    return [f for f in sorted(fs, key=v) if v(f) <= upto]

# ── generator ────────────────────────────────────────────────────────────────
def gen(path, n, seed=1, offset=0, append=False):
    if os.path.exists(path) and not append:
        os.remove(path)
    c = sqlite3.connect(path)
    if not append:
        for f in migrations(28):
            c.executescript(open(f).read())
    rnd = random.Random(seed)
    champs = list(range(1, 172))
    w = [1 / (i + 1) ** 0.8 for i in range(len(champs))]
    legend = list(range(3000, 3090))
    boots = [3006, 3009, 3020, 3047, 3111, 3158]
    patches = ["16.17", "16.18", "16.19", "16.20"]
    pos = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"]
    now = 1792000000000
    c.execute("BEGIN")
    for i in range(n):
        mid = f"OC1_{800000000 + offset + i}"
        patch = patches[min(3, int(i * 4 / n))] if not append else "16.20"
        remake = 1 if rnd.random() < 0.01 else 0
        dur = rnd.randint(900, 2400) if not remake else 200
        c.execute("INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,"
                  " game_duration, remake, facts_version) VALUES (?,?,?,?,?,?,?,?,?,?,4)",
                  (mid, "sea", patch, QID, now - (n - i) * 60000, b"x", 1, now - (n - i) * 60000, dur, remake))
        base = rnd.choice([6, 7, 7, 8, 8, 9])
        picked = rnd.choices(champs, w, k=14)
        seen, ten = set(), []
        for ch in picked:
            if ch not in seen:
                seen.add(ch); ten.append(ch)
        while len(ten) < 10:
            ch = rnd.choice(champs)
            if ch not in seen:
                seen.add(ch); ten.append(ch)
        ten = ten[:10]
        if rnd.random() < 0.03:  # a mirror lane
            ten[5] = ten[0]
        winner = rnd.choice([100, 200])
        for p in range(10):
            team = 100 if p < 5 else 200
            position = pos[p % 5]
            r = rnd.random()
            if r < 0.02: position = None
            elif r < 0.04: position = pos[(p + 1) % 5]  # a shared lane
            t = rnd.random()
            tier = "UNKNOWN" if t < 0.2 else TIERS[max(0, min(9, base + rnd.choice([-1, 0, 0, 1])))]
            items = [rnd.choice(legend) for _ in range(5)] + [0]
            if rnd.random() < 0.1: items[4] = 0
            runes = [8000, rnd.choice([8005, 8008, 8010, 8021]), rnd.choice([8100, 8300, 8400, 8200]),
                     [8008, 9111, 9104, 8017, 8345, 8304], [5005, 5008, 5011]]
            summ = [4, rnd.choice([14, 12, 11, 7, 21, 3])]
            if rnd.random() < 0.5: summ.reverse()
            puuid = f"p{rnd.getrandbits(64):016x}{'x' * 62}"
            win = 1 if team == winner else 0
            k = None if rnd.random() < 0.01 else rnd.randint(0, 15)
            c.execute("INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, kills,"
                      " deaths, assists, items, runes, summoners, facts_version, cs, gold, damage, vision, tier)"
                      " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,4,?,?,?,?,?)",
                      (mid, KS, puuid, team, position, ten[p], win, k, rnd.randint(0, 12), rnd.randint(0, 20),
                       json.dumps(items), json.dumps(runes), json.dumps(summ), rnd.randint(20, 300),
                       rnd.randint(5000, 20000), rnd.randint(5000, 50000), rnd.randint(5, 80), tier))
            if rnd.random() < 0.9:
                fin = rnd.sample(legend, rnd.choice([1, 2, 3, 3, 4, 5]))
                c.execute("INSERT INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills, skill_order,"
                          " builds_version) VALUES (?,?,?,?,?,?,?,?,3)",
                          (mid, KS, puuid, json.dumps(sorted([1055, 2003])), rnd.choice(boots + [None]), json.dumps(fin),
                           "QWEQQRQEQEREEWW", rnd.choice(["QWE", "QEW", "EQW", "WQE"])))
        bans = rnd.sample(champs, 10)
        for b in range(10):
            c.execute("INSERT INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES (?,?,?,?)",
                      (mid, 100 if b < 5 else 200, b + 1, bans[b]))
        if append:
            c.execute("INSERT OR IGNORE INTO analytics_counted (match_id, key_scope, bits) VALUES (?, ?, 0)", (mid, KS))
        if i % 20000 == 19999:
            c.execute("COMMIT"); c.execute("BEGIN"); print("  gen", i + 1, flush=True)
    c.execute("COMMIT")
    if not append: c.execute("ANALYZE")
    c.close()

# ── today's rebuild, ported from src/archive/analytics.rs at 32c4672 ─────────
TIER = "f.tier"
SIDE = "CASE f.team_id WHEN 100 THEN 'blue' ELSE 'red' END"
ON_PLATFORM = "substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'"

def ladder_facts(join, src="match_facts", only=""):
    return f"""FROM {src} f
         JOIN matches m ON m.match_id = f.match_id
         {join}
        WHERE f.key_scope = ?1 AND m.queue_id = ?4 AND m.patch IS NOT NULL AND {ON_PLATFORM}
          AND f.tier IS NOT NULL AND f.team_id IN (100, 200)
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5))) {only}"""

def floors():
    rows = ", ".join(f"({i}, '{t}')" for i, t in enumerate(TIERS))
    return f"floors(rank, tier) AS (VALUES {rows})"

def lanes(extra, src="match_facts", only=""):
    return f"""WITH lane AS (
           SELECT f.match_id, f.puuid, f.team_id, f.position, f.champion_id, f.win, f.tier, m.patch,
             coalesce(m.remake, 0) AS remake{extra},
             count(*) OVER (PARTITION BY f.match_id, f.team_id, f.position) AS n
             FROM {src} f JOIN matches m ON m.match_id = f.match_id
            WHERE f.key_scope = ?1 AND f.position IS NOT NULL AND m.queue_id = ?4 AND m.patch IS NOT NULL
              AND substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'
              AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5))) {only}
         )"""

def one_on_one(join):
    return f"""FROM lane a
         JOIN lane b ON b.match_id = a.match_id AND b.position = a.position AND b.team_id <> a.team_id
         {join}
        WHERE a.n = 1 AND b.n = 1 AND a.champion_id <> b.champion_id AND a.team_id IN (100, 200)
          AND a.tier IS NOT NULL"""

PART_VALUE = """CASE k.part
       WHEN 'item3' THEN CAST(json_extract(p.items, '$[2]') AS TEXT)
       WHEN 'item4' THEN CAST(json_extract(p.items, '$[3]') AS TEXT)
       WHEN 'item5' THEN CAST(json_extract(p.items, '$[4]') AS TEXT)
       WHEN 'starter' THEN nullif(p.starter, '[]')
       WHEN 'boots' THEN CAST(p.boots AS TEXT)
       WHEN 'skill_order' THEN nullif(p.skill_order, '')
       WHEN 'runes' THEN json_extract(p.runes, '$[1]') || ':' || json_extract(p.runes, '$[2]')
       WHEN 'spells' THEN
         min(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]')) || ':'
           || max(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]'))
     END"""

def set_build_players(src="match_facts", only=""):
    facts = ladder_facts("JOIN match_builds b ON b.match_id = f.match_id AND b.key_scope = f.key_scope AND b.puuid = f.puuid", src, only)
    return f"""WITH p AS (
           SELECT m.patch, f.champion_id, coalesce(f.position, '') AS role, coalesce(m.remake, 0) AS remake,
             {TIER} AS tier, {SIDE} AS side, f.win,
             json_array(json_extract(b.items, '$[0]'), json_extract(b.items, '$[1]')) AS core,
             b.items, b.starter, b.boots, b.skill_order, f.runes, f.summoners
           {facts}
             AND json_array_length(b.items) >= 2
         )"""

# Each statement: (table, select producing exactly the table's columns in order).
# `T` is the target (the table itself for a rebuild, temp.d_<table> for a delta).
def champions_sql(src, only):
    facts = ladder_facts("", src, only)
    bfacts = ladder_facts("JOIN match_bans b ON b.match_id = f.match_id", src, only)
    ffacts = ladder_facts("JOIN floors fl ON fl.tier = f.tier", src, only)
    fl = floors()
    return [
        ("analytics_slices", f"""SELECT ?1, ?2, ?3, {TIER}, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
             {facts} GROUP BY {TIER}, m.patch, coalesce(m.remake, 0)""", "key_scope, platform, queue, tier, patch, remake, matches, computed_at"),
        ("analytics_match_totals", f"""SELECT ?1, ?2, ?3, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
             {facts} GROUP BY m.patch, coalesce(m.remake, 0)""", "key_scope, platform, queue, patch, remake, matches, computed_at"),
        ("champion_stats", f"""SELECT ?1, ?2, ?3, {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0),
               {SIDE}, count(*), sum(f.win), count(DISTINCT m.match_id), count(f.kills),
               coalesce(sum(f.kills), 0), coalesce(sum(f.deaths), 0), coalesce(sum(f.assists), 0),
               coalesce(sum(f.cs), 0), coalesce(sum(f.gold), 0), coalesce(sum(f.damage), 0),
               coalesce(sum(f.vision), 0),
               coalesce(sum(m.game_duration) FILTER (WHERE f.kills IS NOT NULL), 0), ?6
             {facts}
             GROUP BY {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0), {SIDE}""",
         "key_scope, platform, queue, tier, patch, champion_id, role, remake, side, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at"),
        ("champion_stats_totals", "@totals", "key_scope, platform, queue, patch, champion_id, role, remake, side, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at"),
        ("champion_bans", f"""SELECT ?1, ?2, ?3, {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
             {bfacts} GROUP BY {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0)""", "key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at"),
        ("champion_ban_totals", f"""SELECT ?1, ?2, ?3, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
             {bfacts} GROUP BY m.patch, b.champion_id, coalesce(m.remake, 0)""", "key_scope, platform, queue, patch, champion_id, remake, bans, computed_at"),
        ("@floor_top", f"""CREATE TEMP TABLE floor_top AS WITH {fl}
             SELECT m.match_id, m.patch, coalesce(m.remake, 0) AS remake, max(fl.rank) AS rank, ?6 AS computed_at
             {ffacts} GROUP BY m.match_id""", None),
        ("analytics_floor_totals", f"""WITH {fl},
               g AS MATERIALIZED (SELECT patch, remake, rank, count(*) AS n FROM temp.floor_top GROUP BY patch, remake, rank)
             SELECT ?1, ?2, ?3, fl.tier, g.patch, g.remake, sum(g.n), ?6
               FROM g JOIN floors fl ON fl.rank <= g.rank GROUP BY fl.tier, g.patch, g.remake""",
         "key_scope, platform, queue, floor_tier, patch, remake, matches, computed_at"),
        ("champion_ban_floor_totals", f"""WITH {fl},
               g AS MATERIALIZED (SELECT t.patch, b.champion_id, t.remake, t.rank, count(DISTINCT t.match_id) AS n
                                    FROM temp.floor_top t JOIN match_bans b ON b.match_id = t.match_id
                                   GROUP BY t.patch, b.champion_id, t.remake, t.rank)
             SELECT ?1, ?2, ?3, fl.tier, g.patch, g.champion_id, g.remake, sum(g.n), ?6
               FROM g JOIN floors fl ON fl.rank <= g.rank GROUP BY fl.tier, g.patch, g.champion_id, g.remake""",
         "key_scope, platform, queue, floor_tier, patch, champion_id, remake, bans, computed_at"),
        ("@drop", "DROP TABLE temp.floor_top", None),
    ]

def matchups_sql(src, only):
    return [("champion_matchups", f"""{lanes("", src, only)}
         SELECT ?1, ?2, ?3, a.patch, a.champion_id, a.position, b.champion_id, a.remake, a.tier,
           CASE a.team_id WHEN 100 THEN 'blue' ELSE 'red' END, count(*), sum(a.win), ?6
           {one_on_one("")}
          GROUP BY a.patch, a.position, a.champion_id, b.champion_id, a.remake, a.tier, a.team_id""",
             "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side, games, wins, computed_at")]

def items_sql(src, only):
    facts = ladder_facts("JOIN json_each(f.items) i", src, only)
    return [("champion_items", f"""SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(DISTINCT f.match_id || ' ' || f.puuid),
               count(DISTINCT CASE WHEN f.win THEN f.match_id || ' ' || f.puuid END), ?6
             {facts}
               AND i.value IS NOT NULL AND i.value <> 0
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER), coalesce(m.remake, 0),
               {TIER}, {SIDE}""", "key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side, games, wins, computed_at")]

def runes_sql(src, only):
    facts = ladder_facts("", src, only)
    return [("champion_runes", f"""SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               json_extract(f.runes, '$[1]'), json_extract(f.runes, '$[2]'),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.runes, '$[1]') IS NOT NULL AND json_extract(f.runes, '$[2]') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), json_extract(f.runes, '$[1]'),
               json_extract(f.runes, '$[2]'), coalesce(m.remake, 0), {TIER}, {SIDE}""",
             "key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side, games, wins, computed_at")]

def spells_sql(src, only):
    facts = ladder_facts("", src, only)
    return [("champion_spells", f"""SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.summoners, '$[0]') IS NOT NULL AND json_extract(f.summoners, '$[1]') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0), {TIER}, {SIDE}""",
             "key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side, games, wins, computed_at")]

def sets_sql(src, only):
    players = set_build_players(src, only)
    lb_lanes = lanes(", f.runes, f.summoners", src, only)
    pairs = one_on_one("JOIN match_builds mb ON mb.match_id = a.match_id AND mb.key_scope = ?1 AND mb.puuid = a.puuid")
    return [
        ("champion_builds", f"""{players}
             SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, tier, side, count(*), sum(win), ?6
               FROM p GROUP BY patch, champion_id, role, remake, core, tier, side""",
         "key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side, games, wins, computed_at"),
        ("champion_build_parts", f"""{players},
             k (part) AS (VALUES ('item3'), ('item4'), ('item5'), ('starter'), ('boots'), ('skill_order'), ('runes'), ('spells')),
             v AS (SELECT p.patch, p.champion_id, p.role, p.remake, p.core, p.tier, p.side, p.win, k.part, {PART_VALUE} AS value
                 FROM p CROSS JOIN k)
             SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, part, value, tier, side, count(*), sum(win), ?6
               FROM v WHERE value IS NOT NULL
              GROUP BY patch, champion_id, role, remake, core, part, value, tier, side""",
         "key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side, games, wins, computed_at"),
        ("@lane_builds", f"""CREATE TEMP TABLE lane_builds AS {lb_lanes}
             SELECT a.patch, a.champion_id, a.position AS role, b.champion_id AS opponent_id, a.remake, a.tier,
               CASE a.team_id WHEN 100 THEN 'blue' ELSE 'red' END AS side, a.win,
               json_array(json_extract(mb.items, '$[0]'), json_extract(mb.items, '$[1]')) AS core,
               mb.items, mb.starter, mb.boots, mb.skill_order, a.runes, a.summoners, ?6 AS computed_at
               {pairs} AND json_array_length(mb.items) >= 2""", None),
        ("champion_matchup_builds", """SELECT ?1, ?2, ?3, patch, champion_id, role, opponent_id, remake, core, tier, side, count(*), sum(win), ?6
           FROM temp.lane_builds GROUP BY patch, champion_id, role, opponent_id, remake, core, tier, side""",
         "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, tier, side, games, wins, computed_at"),
        ("champion_matchup_build_parts", f"""WITH k (part) AS (VALUES ('item3'), ('boots'), ('runes'), ('spells')),
             v AS (SELECT p.patch, p.champion_id, p.role, p.opponent_id, p.remake, p.core, p.tier, p.side, p.win,
                 k.part, {PART_VALUE} AS value FROM temp.lane_builds p CROSS JOIN k)
             SELECT ?1, ?2, ?3, patch, champion_id, role, opponent_id, remake, core, part, value, tier, side, count(*), sum(win), ?6
               FROM v WHERE value IS NOT NULL
              GROUP BY patch, champion_id, role, opponent_id, remake, core, part, value, tier, side""",
         "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, part, value, tier, side, games, wins, computed_at"),
        ("@drop", "DROP TABLE temp.lane_builds", None),
    ]

# The rebuild's six transactions, and the stamp bit each owns.
STEPS = [("champions", 1, champions_sql), ("matchups", 2, matchups_sql), ("items", 4, items_sql),
         ("runes", 8, runes_sql), ("spells", 16, spells_sql), ("sets", 32, sets_sql)]
ALL_BITS = 63
TABLES = ["analytics_slices", "analytics_match_totals", "champion_stats", "champion_stats_totals", "champion_bans",
          "champion_ban_totals", "analytics_floor_totals", "champion_ban_floor_totals", "champion_matchups",
          "champion_items", "champion_runes", "champion_spells", "champion_builds", "champion_build_parts",
          "champion_matchup_builds", "champion_matchup_build_parts"]
COUNT_COL = {"analytics_slices": "matches", "analytics_match_totals": "matches", "analytics_floor_totals": "matches",
             "champion_bans": "bans", "champion_ban_totals": "bans", "champion_ban_floor_totals": "bans"}

def params(now):
    return (KS, PLAT, QUEUE, QID, None, now)

def pk(c, t):
    rows = sorted((r[5], r[1]) for r in c.execute(f"PRAGMA table_info({t})") if r[5] > 0)
    return [n for _, n in rows]

def sums(c, t):
    return [r[1] for r in c.execute(f"PRAGMA table_info({t})") if r[5] == 0 and r[1] != "computed_at"]

TOTALS = """SELECT key_scope, platform, queue, patch, champion_id, role, remake, side,
           sum(games), sum(wins), sum(matches_picked), sum(stated_games), sum(kills), sum(deaths), sum(assists),
           sum(cs), sum(gold), sum(damage), sum(vision), sum(duration_s), ?6
           FROM {src} WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
            AND (?5 IS NULL OR patch IN (SELECT value FROM json_each(?5)))
          GROUP BY patch, champion_id, role, remake, side"""

# The match set a rebuild step counts, for its stamps: every match it read facts of.
STAMP_SET = f"""SELECT DISTINCT m.match_id FROM matches m JOIN match_facts f ON f.match_id = m.match_id
     WHERE f.key_scope = ?1 AND m.queue_id = ?4 AND m.patch IS NOT NULL AND {ON_PLATFORM}
       AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))"""

def rebuild(path, stamp, only=None):
    c = sqlite3.connect(path, isolation_level=None)
    c.execute("PRAGMA journal_mode=WAL"); c.execute("PRAGMA synchronous=NORMAL"); c.execute("PRAGMA cache_size=-65536"); c.execute("PRAGMA mmap_size=268435456"); c.execute(f"PRAGMA wal_autocheckpoint={os.environ.get("CKPT", "1000")}"); c.execute("PRAGMA temp_store=MEMORY") if os.environ.get("TEMPMEM") else None
    out = {}
    for name, bit, fn in STEPS:
        if only and name not in only: continue
        t0 = time.perf_counter()
        c.execute("BEGIN IMMEDIATE")
        now = int(time.time() * 1000)
        for tbl, sql, cols in fn("match_facts", ""):
            if tbl.startswith("@"):
                c.execute(sql, params(now)) if "?" in sql else c.execute(sql)
                continue
            c.execute(f"DELETE FROM {tbl} WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3", (KS, PLAT, QUEUE))
            if sql == "@totals":
                sql = TOTALS.format(src="champion_stats")
            c.execute(f"INSERT INTO {tbl} ({cols}) {sql}", params(now))
        if stamp:
            c.execute(f"""INSERT INTO analytics_counted (match_id, key_scope, bits) {STAMP_SET.replace('SELECT DISTINCT m.match_id', 'SELECT DISTINCT m.match_id, ?1, ' + str(bit))}
                          ON CONFLICT (match_id, key_scope) DO UPDATE SET bits = bits | {bit} WHERE bits & {bit} = 0 AND ?6 IS NOT NULL""",
                      params(now))
            c.execute(f"DELETE FROM analytics_retract WHERE step = {bit} AND key_scope = ?1 AND match_id IN ({STAMP_SET})", params(now)[:5])
        c.execute("COMMIT")
        out[name] = time.perf_counter() - t0
        print(f"  {name:9s} {out[name]*1000:9.0f} ms", flush=True)
    if stamp:
        c.execute("INSERT OR REPLACE INTO analytics_stamped (key_scope, platform, queue, at) VALUES (?,?,?,?)",
                  (KS, PLAT, QUEUE, int(time.time() * 1000)))
    print(f"  total     {sum(out.values())*1000:9.0f} ms")
    return out

# ── V0029 prototype ──────────────────────────────────────────────────────────
V0029 = """
CREATE TABLE analytics_counted (
  match_id TEXT NOT NULL, key_scope TEXT NOT NULL,
  bits INTEGER NOT NULL,                 -- the rebuild steps / delta groups that count this match
  PRIMARY KEY (match_id, key_scope)
) WITHOUT ROWID;
CREATE INDEX analytics_uncounted ON analytics_counted (key_scope, match_id) WHERE bits <> 63;
CREATE TABLE analytics_retract (
  match_id TEXT NOT NULL, key_scope TEXT NOT NULL, puuid TEXT NOT NULL,
  step INTEGER NOT NULL,                  -- one step bit
  tier TEXT,                             -- the tier that step counted the player at
  PRIMARY KEY (match_id, key_scope, puuid, step)
) WITHOUT ROWID;
CREATE TABLE analytics_stamped (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue)
) WITHOUT ROWID;
CREATE TRIGGER match_facts_tier_moved AFTER UPDATE OF tier ON match_facts
  WHEN OLD.tier IS NOT NEW.tier
BEGIN
  INSERT OR IGNORE INTO analytics_retract (match_id, key_scope, puuid, step, tier)
  SELECT OLD.match_id, OLD.key_scope, OLD.puuid, g.bit, OLD.tier
    FROM analytics_counted c
    JOIN (SELECT 1 AS bit UNION ALL SELECT 2 UNION ALL SELECT 4 UNION ALL SELECT 8 UNION ALL SELECT 16 UNION ALL SELECT 32) g
      ON c.bits & g.bit
   WHERE c.match_id = OLD.match_id AND c.key_scope = OLD.key_scope;
END;
"""

def migrate(path):
    c = sqlite3.connect(path, isolation_level=None)
    t0 = time.perf_counter()
    c.execute("BEGIN"); c.executescript(V0029) if False else None
    for stmt in split_sql(V0029):
        c.execute(stmt)
    c.execute("COMMIT")
    print(f"  V0029 {1000*(time.perf_counter()-t0):.1f} ms")

def split_sql(s):
    out, cur = [], []
    for line in s.strip().splitlines():
        cur.append(line)
        txt = "\n".join(cur)
        if line.rstrip().endswith(";") and (not txt.lstrip().upper().startswith("CREATE TRIGGER") or line.strip() == "END;"):
            out.append(txt); cur = []
    return out

# ── delta ────────────────────────────────────────────────────────────────────
def delta(path, batch, quiet=False, max_batches=None, sizes=None):
    c = sqlite3.connect(path, isolation_level=None)
    c.execute("PRAGMA journal_mode=WAL"); c.execute("PRAGMA synchronous=NORMAL"); c.execute("PRAGMA cache_size=-65536"); c.execute("PRAGMA mmap_size=268435456"); c.execute(f"PRAGMA wal_autocheckpoint={os.environ.get("CKPT", "1000")}"); c.execute("PRAGMA temp_store=MEMORY") if os.environ.get("TEMPMEM") else None
    cols = {t: (pk(c, t), sums(c, t)) for t in TABLES}
    times, n_total, b_i = [], 0, 0
    while True:
        size = sizes[b_i % len(sizes)] if sizes else batch
        # Read on a "reader" (outside the write tx): pending ids, oldest first.
        ids = [r[0] for r in c.execute(
            """SELECT c.match_id FROM analytics_counted c INDEXED BY analytics_uncounted
                WHERE c.key_scope = ?1 AND c.bits <> 63 ORDER BY c.match_id LIMIT ?2""", (KS, size))]
        if len(ids) < size:
            ids += [r[0] for r in c.execute("SELECT DISTINCT match_id FROM analytics_retract WHERE key_scope = ?1 LIMIT ?2",
                                            (KS, size - len(ids))) if r[0] not in set(ids)]
        if not ids or (max_batches is not None and b_i >= max_batches):
            break
        t0 = time.perf_counter()
        c.execute("BEGIN IMMEDIATE")
        now = int(time.time() * 1000)
        c.execute("CREATE TEMP TABLE batch (match_id TEXT PRIMARY KEY, bits INTEGER NOT NULL)")
        c.executemany("INSERT INTO temp.batch SELECT ?1, coalesce((SELECT bits FROM analytics_counted WHERE match_id = ?1 AND key_scope = ?2), 0)",
                      [(i, KS) for i in ids])
        # The batch's facts as they are now.
        c.execute("CREATE TEMP TABLE df AS SELECT f.* FROM temp.batch b CROSS JOIN match_facts f ON f.match_id = b.match_id AND f.key_scope = ?1", (KS,))
        for t in TABLES:
            c.execute(f"CREATE TEMP TABLE d_{t} AS SELECT * FROM {t} WHERE 0")
        negs = c.execute("SELECT 1 FROM analytics_retract WHERE key_scope = ?1 AND match_id IN (SELECT match_id FROM temp.batch) LIMIT 1", (KS,)).fetchone() is not None
        for name, bit, fn in STEPS:
            # Subtract what this step counted for a match whose tier moved since: its facts at the tiers it counted.
            r = c.execute("SELECT count(*) FROM analytics_retract WHERE key_scope = ?1 AND step = ?2", (KS, bit)).fetchone()[0]
            if r:
                c.execute(f"""CREATE TEMP TABLE rf AS SELECT f.match_id, f.key_scope, f.puuid, f.team_id, f.position, f.champion_id, f.win,
                                f.kills, f.deaths, f.assists, f.items, f.runes, f.summoners, f.facts_version, f.cs, f.gold, f.damage, f.vision,
                                CASE WHEN r.match_id IS NOT NULL THEN r.tier ELSE f.tier END AS tier
                                FROM temp.df f LEFT JOIN analytics_retract r ON r.match_id = f.match_id AND r.key_scope = f.key_scope
                                 AND r.puuid = f.puuid AND r.step = {bit}
                               WHERE f.match_id IN (SELECT match_id FROM analytics_retract WHERE key_scope = ?1 AND step = {bit})
                                 AND f.match_id IN (SELECT match_id FROM temp.batch)""", (KS,))
                apply_step(c, fn, "temp.rf", "", -1, now)
                c.execute("DROP TABLE temp.rf")
            # Add the step's count for every batch match it doesn't count yet, or whose count it just took back.
            only = f"""AND (f.match_id IN (SELECT match_id FROM temp.batch WHERE bits & {bit} = 0)
                         OR f.match_id IN (SELECT match_id FROM analytics_retract WHERE key_scope = ?1 AND step = {bit}))"""
            if negs:
                apply_step(c, fn, "temp.df", only, +1, now)
            else:
                upsert_step(c, fn, "temp.df", only, now, cols)
        for t in TABLES:
            p, s = cols[t]
            pkl = ", ".join(p)
            sets = ", ".join(f"{x} = {x} + excluded.{x}" for x in s) + ", computed_at = excluded.computed_at"
            c.execute(f"""INSERT INTO {t} ({pkl}, {', '.join(s)}, computed_at)
                          SELECT {pkl}, {', '.join(f'sum({x})' for x in s)}, max(computed_at) FROM temp.d_{t} GROUP BY {pkl}
                          ON CONFLICT ({pkl}) DO UPDATE SET {sets}""")
            cc = COUNT_COL.get(t, "games")
            if negs: c.execute(f"DELETE FROM {t} WHERE {cc} = 0 AND ({pkl}) IN (SELECT {pkl} FROM temp.d_{t})")
            c.execute(f"DROP TABLE temp.d_{t}")
        c.execute("""INSERT INTO analytics_counted (match_id, key_scope, bits) SELECT match_id, ?1, 63 FROM temp.batch WHERE true
                     ON CONFLICT (match_id, key_scope) DO UPDATE SET bits = 63""", (KS,))
        c.execute("DELETE FROM analytics_retract WHERE key_scope = ?1 AND match_id IN (SELECT match_id FROM temp.batch)", (KS,))
        c.execute("DROP TABLE temp.batch"); c.execute("DROP TABLE temp.df")
        c.execute("COMMIT")
        dt = time.perf_counter() - t0
        times.append(dt); n_total += len(ids); b_i += 1
        if not quiet and b_i % 20 == 0:
            print(f"  batch {b_i}: {len(ids)} matches {dt*1000:.0f} ms", flush=True)
    if times:
        ts = sorted(times)
        print(f"  delta: {n_total} matches in {len(times)} tx; tx ms p50 {1000*statistics.median(ts):.0f}"
              f" p90 {1000*ts[int(.9*(len(ts)-1))]:.0f} max {1000*ts[-1]:.0f}; sum {1000*sum(ts):.0f} ms")
    return times

def upsert_set(t, cols):
    p, s = cols[t]
    pkl = ", ".join(p)
    return pkl, ", ".join(f"{x} = {x} + excluded.{x}" for x in s) + ", computed_at = excluded.computed_at"

def upsert_step(c, fn, src, only, now, cols):
    """The add-only path: each INSERT ... SELECT upserts straight into its table."""
    for tbl, sql, cl in fn(src, only):
        if tbl.startswith("@"):
            c.execute(sql, params(now)) if "?" in sql else c.execute(sql)
            continue
        if sql == "@totals":
            continue
        pkl, sets = upsert_set(tbl, cols)
        if tbl == "champion_stats":
            c.execute(f"CREATE TEMP TABLE pos AS SELECT * FROM {tbl} WHERE 0")
            c.execute(f"INSERT INTO temp.pos ({cl}) {sql}", params(now))
            c.execute(f"INSERT INTO {tbl} SELECT * FROM temp.pos WHERE true ON CONFLICT ({pkl}) DO UPDATE SET {sets}")
            add_totals(c, "temp.pos", +1, now)
            c.execute("DROP TABLE temp.pos")
        else:
            c.execute(f"INSERT INTO {tbl} ({cl}) {sql} ON CONFLICT ({pkl}) DO UPDATE SET {sets}", params(now))

def apply_step(c, fn, src, only, sign, now):
    for tbl, sql, cols in fn(src, only):
        if tbl.startswith("@"):
            c.execute(sql, params(now)) if "?" in sql else c.execute(sql)
            continue
        if sql == "@totals":
            sql = TOTALS.format(src="temp.d_champion_stats_sign")
            # the stats rows this pass just produced, summed over tier
            continue
        if sign < 0:
            c.execute(f"CREATE TEMP TABLE neg AS SELECT * FROM {tbl} WHERE 0")
            c.execute(f"INSERT INTO temp.neg ({cols}) {sql}", params(now))
            ss = sums(c, tbl)
            setneg = ", ".join(f"-{x} AS {x}" if x in ss else x for x in [r[1] for r in c.execute(f"PRAGMA table_info({tbl})")])
            c.execute(f"INSERT INTO temp.d_{tbl} SELECT {setneg} FROM temp.neg")
            if tbl == "champion_stats":
                add_totals(c, "temp.neg", -1, now)
            c.execute("DROP TABLE temp.neg")
        else:
            c.execute(f"CREATE TEMP TABLE pos AS SELECT * FROM {tbl} WHERE 0")
            c.execute(f"INSERT INTO temp.pos ({cols}) {sql}", params(now))
            c.execute(f"INSERT INTO temp.d_{tbl} SELECT * FROM temp.pos")
            if tbl == "champion_stats":
                add_totals(c, "temp.pos", +1, now)
            c.execute("DROP TABLE temp.pos")

def add_totals(c, src, sign, now):
    s = sums(c, "champion_stats_totals")
    m = "-" if sign < 0 else ""
    c.execute(f"""INSERT INTO temp.d_champion_stats_totals (key_scope, platform, queue, patch, champion_id, role, remake, side,
                    {', '.join(s)}, computed_at)
                  SELECT key_scope, platform, queue, patch, champion_id, role, remake, side,
                    {', '.join(f'{m}sum({x})' for x in s)}, ?6
                    FROM {src} GROUP BY key_scope, platform, queue, patch, champion_id, role, remake, side""", params(now))

# ── checks ───────────────────────────────────────────────────────────────────
def diff(a, b):
    ca, cb = sqlite3.connect(a), sqlite3.connect(b)
    bad = 0
    for t in TABLES:
        cols = [r[1] for r in ca.execute(f"PRAGMA table_info({t})") if r[1] != "computed_at"]
        q = f"SELECT {', '.join(cols)} FROM {t} ORDER BY {', '.join(cols)}"
        ra, rb = ca.execute(q).fetchall(), cb.execute(q).fetchall()
        same = ra == rb
        bad += not same
        print(f"  {t:30s} {len(ra):9d} {len(rb):9d} {'same' if same else 'DIFFERENT'}")
        if not same:
            sa, sb = set(ra), set(rb)
            print("    only in A:", list(sa - sb)[:3]); print("    only in B:", list(sb - sa)[:3])
    return bad

def late(path, pct, seed=2):
    c = sqlite3.connect(path, isolation_level=None)
    rnd = random.Random(seed)
    rows = c.execute("SELECT match_id, puuid FROM match_facts WHERE tier = 'UNKNOWN'").fetchall()
    pick = rnd.sample(rows, int(len(rows) * pct / 100))
    t0 = time.perf_counter()
    c.execute("BEGIN IMMEDIATE")
    c.executemany("UPDATE match_facts SET tier = 'MASTER' WHERE match_id = ? AND puuid = ? AND tier = 'UNKNOWN'", pick)
    c.execute("COMMIT")
    n = c.execute("SELECT count(*), count(DISTINCT match_id) FROM analytics_retract").fetchone()
    print(f"  late-stamped {len(pick)} facts in {1000*(time.perf_counter()-t0):.0f} ms; retract rows {n[0]} over {n[1]} matches")

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "gen": gen(sys.argv[2], int(sys.argv[3]))
    elif cmd == "rebuild": rebuild(sys.argv[2], False)
    elif cmd == "rebuild-stamp": rebuild(sys.argv[2], True)
    elif cmd == "delta": delta(sys.argv[2], int(sys.argv[3]))
    elif cmd == "diff": sys.exit(1 if diff(sys.argv[2], sys.argv[3]) else 0)
    elif cmd == "migrate": migrate(sys.argv[2])
    elif cmd == "arrive": gen(sys.argv[2], int(sys.argv[3]), seed=int(sys.argv[4]), offset=10_000_000 * int(sys.argv[4]), append=True)
    elif cmd == "deltas":
        ts = delta(sys.argv[2], int(sys.argv[3]), quiet=True)
        print("  over 250 ms:", sum(t > .25 for t in ts), "of", len(ts), "; in order:", [round(t*1000) for t in ts])
    elif cmd == "rebuild-steps": rebuild(sys.argv[2], True, sys.argv[3].split(","))
    elif cmd == "pending":
        c = sqlite3.connect(sys.argv[2], isolation_level=None)
        c.execute("INSERT OR IGNORE INTO analytics_counted (match_id, key_scope, bits) SELECT DISTINCT match_id, key_scope, 0 FROM match_facts")
    elif cmd == "late": late(sys.argv[2], float(sys.argv[3]))

#!/usr/bin/env python3
"""THR-01 step 0: sample a running proxy during a crawl's archive stage.

Every --interval seconds it reads `/v1/admin/metrics` (the limiter's windows
per scope) and `/metrics` (Prometheus counters and histograms). On Ctrl-C or
after --duration seconds it prints, per match-v5 region:

- app-bucket usage as a share of the limit, against the bulk ceiling: the
  median, the 90th percentile and how often it was at 90 % of the ceiling or
  more (THR-01's "measure first" and its Accept);
- the `match.byId` method windows the same way;
- requests sent per (region, method) and matches archived per second;
- `match.byId` round-trip time per region (mean, and the median read off
  the histogram buckets, so it is coarse: 0.1 / 0.25 / 0.5 s edges);
- bulk limiter waits per region: mean, and how many went past the yield
  budget (`JOB_YIELD_BUDGET_MS`, 1 s by default).

Standard library only. Usage:

    ADMIN_KEY=... scripts/thr01-baseline.py --base http://localhost:8080 \
        --ceiling 0.95 --interval 5 --duration 1800 > baseline.txt

The ceiling is `BULK_IDLE_CEILING` while nobody is using the app (0.95 by
default) and `BULK_USAGE_CEILING` (0.80) otherwise; the script cannot tell
which applied, so pass the one you expect and read both columns.
"""

import argparse
import json
import os
import re
import statistics
import sys
import time
import urllib.request

LABEL = re.compile(r'(\w+)="((?:[^"\\]|\\.)*)"')


def get(base, path, key):
    req = urllib.request.Request(base.rstrip("/") + path)
    if key:
        req.add_header("Authorization", f"Bearer {key}")
    with urllib.request.urlopen(req, timeout=10) as r:
        return r.read().decode()


def parse_prometheus(text):
    """{(name, frozenset(labels)): value} for every sample line."""
    out = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        head, _, value = line.rpartition(" ")
        if "{" in head:
            name, _, rest = head.partition("{")
            labels = frozenset(LABEL.findall(rest))
        else:
            name, labels = head, frozenset()
        try:
            out[(name, labels)] = float(value)
        except ValueError:
            pass
    return out


def delta(a, b):
    return {k: b[k] - a.get(k, 0.0) for k in b}


def select(samples, name, **want):
    for (n, labels), v in samples.items():
        d = dict(labels)
        if n == name and all(d.get(k) == w for k, w in want.items()):
            yield d, v


def histogram(samples, name, **want):
    """(buckets [(le, count)], sum, count) summed over every other label."""
    buckets, total, count = {}, 0.0, 0.0
    for d, v in select(samples, name + "_bucket", **want):
        le = float("inf") if d["le"] == "+Inf" else float(d["le"])
        buckets[le] = buckets.get(le, 0.0) + v
    for _, v in select(samples, name + "_sum", **want):
        total += v
    for _, v in select(samples, name + "_count", **want):
        count += v
    return sorted(buckets.items()), total, count


def quantile(buckets, count, q):
    """Linear interpolation inside the bucket that holds the q-th sample."""
    if count <= 0:
        return None
    rank, lo, below = q * count, 0.0, 0.0
    for le, cum in buckets:
        if cum >= rank:
            if le == float("inf"):
                return lo
            inside = cum - below
            return lo + (le - lo) * ((rank - below) / inside if inside else 0.0)
        lo, below = le, cum
    return lo


def share(windows):
    """The binding window: the highest used/limit."""
    shares = [w["used"] / w["limit"] for w in windows if w.get("limit")]
    return max(shares) if shares else None


def summary(xs, ceiling):
    if not xs:
        return "no samples"
    xs = sorted(xs)
    p90 = xs[min(len(xs) - 1, int(0.9 * len(xs)))]
    at = sum(1 for x in xs if x >= 0.9 * ceiling) / len(xs)
    return (f"median {statistics.median(xs):.2f}, p90 {p90:.2f}, max {xs[-1]:.2f} of the limit; "
            f"{at:.0%} of {len(xs)} samples at >= 90% of the {ceiling} ceiling")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default="http://localhost:8080")
    ap.add_argument("--key", default=os.environ.get("ADMIN_KEY"))
    ap.add_argument("--interval", type=float, default=5.0)
    ap.add_argument("--duration", type=float, default=0.0, help="seconds; 0 runs until Ctrl-C")
    ap.add_argument("--ceiling", type=float, default=0.95)
    ap.add_argument("--yield-budget", type=float, default=1.0, help="JOB_YIELD_BUDGET_MS in seconds")
    args = ap.parse_args()

    first = parse_prometheus(get(args.base, "/metrics", args.key))
    t0 = time.time()
    app, method = {}, {}
    last = first
    try:
        while not args.duration or time.time() - t0 < args.duration:
            snap = json.loads(get(args.base, "/v1/admin/metrics", args.key))
            for s in snap.get("limiter", []):
                if s.get("kind") != "region":
                    continue
                if (x := share(s.get("windows", []))) is not None:
                    app.setdefault(s["scope"], []).append(x)
                for m in s.get("methods", []):
                    if m.get("method") == "match.byId" and (x := share(m.get("windows", []))) is not None:
                        method.setdefault(s["scope"], []).append(x)
            last = parse_prometheus(get(args.base, "/metrics", args.key))
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass
    elapsed = time.time() - t0
    d = delta(first, last)

    print(f"# THR-01 baseline: {elapsed:.0f} s sampled every {args.interval} s from {args.base}")
    print("\n## App-bucket usage per region (binding window)")
    for scope in sorted(app):
        print(f"- {scope}: {summary(app[scope], args.ceiling)}")
    print("\n## match.byId method windows per region")
    for scope in sorted(method):
        print(f"- {scope}: {summary(method[scope], args.ceiling)}")

    archived = sum(v for _, v in select(d, "proxy_archived_matches_total"))
    print(f"\n## Throughput\n- matches archived: {archived:.0f} ({archived / elapsed:.1f}/s)")
    reqs = {}
    for lab, v in select(d, "proxy_upstream_requests_total"):
        reqs[(lab.get("region"), lab.get("method"))] = reqs.get((lab.get("region"), lab.get("method")), 0) + v
    for (region, m), v in sorted(reqs.items()):
        if v:
            print(f"- {region} {m}: {v:.0f} requests ({v / elapsed:.1f}/s)")
    jobs = {}
    for lab, v in select(d, "proxy_jobs_total"):
        jobs[(lab.get("job"), lab.get("status"))] = v
    for (job, status), v in sorted(jobs.items()):
        if v:
            print(f"- jobs {job} {status}: {v:.0f}")

    print("\n## match.byId round trip per region")
    regions = sorted({lab.get("region") for lab, _ in select(d, "proxy_upstream_latency_seconds_count", method="match.byId")})
    for region in regions:
        b, total, n = histogram(d, "proxy_upstream_latency_seconds", method="match.byId", region=region)
        if n:
            p50 = quantile(b, n, 0.5)
            print(f"- {region}: {n:.0f} calls, mean {total / n * 1000:.0f} ms, median ~{p50 * 1000:.0f} ms (bucketed)")

    print("\n## Bulk limiter waits per region")
    regions = sorted({lab.get("region") for lab, _ in select(d, "proxy_rl_wait_seconds_count", priority="bulk")})
    for region in regions:
        b, total, n = histogram(d, "proxy_rl_wait_seconds", priority="bulk", region=region)
        if n:
            within = max((c for le, c in b if le <= args.yield_budget), default=0.0)
            print(f"- {region}: {n:.0f} waits, mean {total / n * 1000:.0f} ms, "
                  f"{n - within:.0f} past the {args.yield_budget} s budget")
    return 0


if __name__ == "__main__":
    sys.exit(main())

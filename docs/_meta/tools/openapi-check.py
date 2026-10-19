#!/usr/bin/env python3
"""Compare the generated OpenAPI document with the docs copy.

Usage: openapi-check.py <generated.json> <docs/api/openapi.json>
Every path, method, parameter name and response code must agree, and info.version must match.
The one tolerated difference is the docs copy renaming the duplicate operationId `champions`
on GET /v1/players/{puuid}/champions to `player_champions` (see docs/_meta/doc-maintenance.md).
"""
import json, sys
gen, doc = (json.load(open(p, encoding="utf-8")) for p in sys.argv[1:3])
errors = []
if gen["info"]["version"] != doc["info"]["version"]:
    errors.append(f"info.version {doc['info']['version']} in docs, {gen['info']['version']} generated")
gp, dp = gen["paths"], doc["paths"]
for p in sorted(set(gp) | set(dp)):
    if p not in gp: errors.append(f"{p}: in docs only"); continue
    if p not in dp: errors.append(f"{p}: generated but missing from docs"); continue
    for m in sorted(set(gp[p]) | set(dp[p])):
        if m not in gp[p] or m not in dp[p]:
            errors.append(f"{m.upper()} {p}: present on one side only"); continue
        g, d = gp[p][m], dp[p][m]
        gid, did = g.get("operationId"), d.get("operationId")
        if gid != did and not (p == "/v1/players/{puuid}/champions" and gid == "champions" and did == "player_champions"):
            errors.append(f"{m.upper()} {p}: operationId {gid} vs {did}")
        gparams = sorted((x.get("name"), x.get("in")) for x in g.get("parameters", []))
        dparams = sorted((x.get("name"), x.get("in")) for x in d.get("parameters", []))
        if gparams != dparams: errors.append(f"{m.upper()} {p}: parameters differ")
        if sorted(g.get("responses", {})) != sorted(d.get("responses", {})):
            errors.append(f"{m.upper()} {p}: response codes differ")
if sorted(gen.get("components", {}).get("schemas", {})) != sorted(doc.get("components", {}).get("schemas", {})):
    errors.append("components.schemas names differ")
for e in errors: print(f"::error::{e}")
print(f"openapi-check: {len(gp)} paths compared, {len(errors)} differences")
sys.exit(1 if errors else 0)

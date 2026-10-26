#!/usr/bin/env python3
"""The documentation gate (CI job `docs`).

Usage: docs-guard.py <base-ref> [<head-ref>]   (head defaults to HEAD)

Fails when, between base and head:
  1. a file a page's `**Sources:**` footer cites changed, but the page itself did not;
  2. src/, deploy/, .github/, Cargo.toml or .env.example changed and nothing under docs/ did;
  3. any page's `**Last verified:** … against commit `<sha>`` names a commit not in history,
     or a page changed in this range still cites a commit older than base (footer not refreshed).
Pages under docs/design, docs/flt, docs/lad, docs/perf, docs/thr and the repo's own records (DECISIONS, IMPLEMENTATION,
CUTOVER, contract) are exempt from 1 and 3: they are the design authority, not this set. A page that
cites PROGRESS.md, DECISIONS.md, IMPLEMENTATION.md or CUTOVER.md is not stale when those move (rule 1).
"""
import os, re, subprocess, sys

base = sys.argv[1]
head = sys.argv[2] if len(sys.argv) > 2 else "HEAD"
EXEMPT = ("docs/design/", "docs/flt/", "docs/lad/", "docs/perf/", "docs/thr/", "docs/contract/", "docs/adr/", "docs/img/",
          "docs/DECISIONS.md", "docs/IMPLEMENTATION.md", "docs/CUTOVER.md", "docs/_meta/scratch/")
CODE_PREFIXES = ("src/", "deploy/", ".github/", "Cargo.toml", ".env.example")
# Record files change with every task (ticks, ADRs); a page citing them for context is not stale when they move.
RECORDS = ("PROGRESS.md", "docs/DECISIONS.md", "docs/IMPLEMENTATION.md", "docs/CUTOVER.md")

def git(*a):
    return subprocess.run(["git", *a], capture_output=True, text=True, check=True).stdout

def exempt(p):
    return any(p.startswith(e) for e in EXEMPT)

changed = set(git("diff", "--name-only", f"{base}...{head}").split())
pages = [os.path.join(dp, f) for dp, _, fn in os.walk("docs") for f in fn
         if f.endswith(".md") and not exempt(os.path.join(dp, f))]
errors = []

# rule 2
if any(p.startswith(CODE_PREFIXES) for p in changed) and not any(p.startswith("docs/") for p in changed):
    errors.append("code changed (src/, deploy/, .github/, Cargo.toml or .env.example) but nothing under docs/ did; "
                  "update the pages named in docs/_meta/doc-maintenance.md or state why in the PR")

src_re = re.compile(r"^\*\*Sources:\*\*(.*)$", re.M)
ver_re = re.compile(r"^\*\*Last verified:\*\* \d{4}-\d{2}-\d{2} against commit `([0-9a-f]{7,40})`", re.M)
path_re = re.compile(r"`([^`:]+?)(?::L\d+(?:-L\d+)?)?`")
for page in sorted(pages):
    text = open(page, encoding="utf-8").read()
    m = src_re.search(text)
    v = ver_re.search(text)
    if not m or not v:
        errors.append(f"{page}: missing the Sources or Last verified footer")
        continue
    # rule 1
    cited = {c.strip() for c in path_re.findall(m.group(1))}
    stale = sorted(c for c in cited if c in changed and not c.startswith("docs/") and c not in RECORDS)
    if stale and page not in changed:
        errors.append(f"{page}: cites {', '.join(stale)}, which changed, but the page did not")
    # rule 3
    sha = v.group(1)
    if subprocess.run(["git", "cat-file", "-e", f"{sha}^{{commit}}"], capture_output=True).returncode != 0:
        errors.append(f"{page}: Last verified names unknown commit {sha}")
    elif page in changed:
        older = subprocess.run(["git", "merge-base", "--is-ancestor", sha, base], capture_output=True).returncode == 0
        same = git("rev-parse", sha).strip() == git("rev-parse", base).strip()
        if older and not same:
            errors.append(f"{page}: changed in this range but Last verified still cites {sha}, older than {base}")

for e in errors:
    print(f"::error::{e}")
print(f"docs-guard: {len(pages)} pages checked, {len(changed)} files changed, {len(errors)} problems")
sys.exit(1 if errors else 0)

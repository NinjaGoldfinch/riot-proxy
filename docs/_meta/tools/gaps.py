#!/usr/bin/env python3
"""Collect every `TODO (unverified)` blockquote under docs/ (excluding _meta/scratch and gaps.md) as file | line | text."""
import os,re,sys
root='docs'; rows=[]
for dp,dn,fn in os.walk(root):
    if '/scratch' in dp: continue
    for f in sorted(fn):
        if not f.endswith('.md') or f=='gaps.md': continue
        p=os.path.join(dp,f)
        for ln,line in enumerate(open(p,encoding='utf-8').read().splitlines(),1):
            if 'TODO (unverified)' in line:
                txt=re.sub(r'^>\s*\*\*TODO \(unverified\):\*\*\s*','',line.strip())
                rows.append((p[len('docs/'):],ln,txt))
for r in rows: print(f"{r[0]}\t{r[1]}\t{r[2]}")
print(len(rows),file=sys.stderr)

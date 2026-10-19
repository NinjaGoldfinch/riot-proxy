#!/usr/bin/env python3
"""Check every relative Markdown link under docs/ resolves (file exists; #anchor exists when the target is .md)."""
import os, re, sys, unicodedata
root = sys.argv[1] if len(sys.argv) > 1 else 'docs'
link_re = re.compile(r'(?<!\!)\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)')
fence_re = re.compile(r'^```')
def anchors(path):
    out=set()
    try:
        txt=open(path,encoding='utf-8').read()
    except Exception:
        return out
    infence=False
    for line in txt.splitlines():
        if fence_re.match(line): infence=not infence; continue
        if infence: continue
        m=re.match(r'^(#{1,6})\s+(.*)$',line)
        if m:
            h=m.group(2).strip()
            h=re.sub(r'`','',h)
            h=re.sub(r'\[([^\]]*)\]\([^)]*\)',r'\1',h)
            h=h.lower()
            h=re.sub(r'[^\w\- ]','',h)
            h=h.replace(' ','-')
            out.add(h)
    return out
bad=0; total=0
for dp,dn,fn in os.walk(root):
    if '/scratch' in dp: continue
    for f in fn:
        if not f.endswith('.md'): continue
        p=os.path.join(dp,f)
        txt=open(p,encoding='utf-8').read()
        infence=False
        for ln,line in enumerate(txt.splitlines(),1):
            if fence_re.match(line): infence=not infence; continue
            if infence: continue
            for m in link_re.finditer(line):
                tgt=m.group(1)
                if tgt.startswith(('http://','https://','mailto:')): continue
                total+=1
                path,_,frag=tgt.partition('#')
                if path=='':
                    full=p
                else:
                    full=os.path.normpath(os.path.join(dp,path))
                if not os.path.exists(full):
                    print(f'{p}:{ln}: missing {tgt}'); bad+=1; continue
                if frag and full.endswith('.md'):
                    if frag not in anchors(full):
                        print(f'{p}:{ln}: no anchor #{frag} in {os.path.relpath(full)}'); bad+=1
print(f'checked {total} relative links, {bad} broken')
sys.exit(1 if bad else 0)

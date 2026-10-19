#!/usr/bin/env bash
# Extract every ```mermaid block under a directory and render each with mermaid-cli. Prints PASS/FAIL per block.
# On the dev box: LD_LIBRARY_PATH=$(cat ~/.local/chromelibs/libpath) FONTCONFIG_FILE=~/.local/chromelibs/fonts.conf \
#   PUPPETEER_SKIP_DOWNLOAD=1 MERMAID_CHROME=~/.cache/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell docs/_meta/tools/mermaidcheck.sh docs
set -u
root=${1:-docs}
out=${MERMAID_OUT:-/tmp/ninja-docs-mermaid}; rm -rf "$out"; mkdir -p "$out"
# MERMAID_CHROME: a Chromium binary to use (the dev box has no system Chrome); unset lets puppeteer use its own.
if [ -n "${MERMAID_CHROME:-}" ]; then
  printf '{"executablePath":"%s","args":["--no-sandbox","--disable-gpu"]}\n' "$MERMAID_CHROME" > "$out/pp.json"
else
  printf '{"args":["--no-sandbox","--disable-gpu"]}\n' > "$out/pp.json"
fi
python3 - "$root" "$out" <<'PY'
import os,re,sys
root,out=sys.argv[1],sys.argv[2]
n=0
for dp,dn,fn in os.walk(root):
    if '/scratch' in dp: continue
    for f in sorted(fn):
        if not f.endswith('.md'): continue
        p=os.path.join(dp,f); txt=open(p,encoding='utf-8').read()
        for i,m in enumerate(re.finditer(r'```mermaid\n(.*?)```',txt,re.S)):
            n+=1
            name=p.replace('/','__').replace('.md','')+f'__{i+1}.mmd'
            open(os.path.join(out,name),'w').write(m.group(1))
print(n)
PY
fails=0; total=0
for f in "$out"/*.mmd; do
  total=$((total+1))
  if npx -y @mermaid-js/mermaid-cli@11 -p "$out/pp.json" -i "$f" -o "${f%.mmd}.svg" >"$f.log" 2>&1; then
    echo "PASS $(basename "$f")"
  else
    echo "FAIL $(basename "$f")"; tail -5 "$f.log"; fails=$((fails+1))
  fi
done
echo "rendered $total blocks, $fails failed"

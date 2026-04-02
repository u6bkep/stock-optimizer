#!/usr/bin/env bash
set -e

echo "Checking Rust..."
cargo test 2>&1

echo "Checking JS syntax..."
python3 -c "
import re
html = open('index.html').read()
m = re.search(r'<script type=\"module\">(.*?)</script>', html, re.DOTALL)
if m:
    with open('/tmp/_check.mjs', 'w') as f:
        f.write(m.group(1))
" && node --check /tmp/_check.mjs

echo "All checks passed."

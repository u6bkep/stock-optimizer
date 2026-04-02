#!/usr/bin/env bash
set -e

wasm-pack build --target web --out-dir pkg
echo "Serving at http://localhost:8000"
python3 -c "
import http.server

class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        '.wasm': 'application/wasm',
        '.js': 'application/javascript',
    }

http.server.HTTPServer(('', 8000), Handler).serve_forever()
"

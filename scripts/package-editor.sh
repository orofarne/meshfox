#!/usr/bin/env bash
# Builds the in-browser editor into a directory that is uploaded to Cloudflare
# Pages as it is:
#
#   scripts/package-editor.sh [out-dir]        (default: editor-bundle)
#
# A plain static site (relative URLs) plus `_headers`, which Cloudflare Pages
# reads itself: the security headers (Content-Security-Policy, ...) and the
# cache rules. Cloudflare compresses (brotli) and serves .wasm correctly on
# its own. Serve it from its own origin (a subdomain): drafts live in that
# origin's IndexedDB.
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:-editor-bundle}"
export PATH="$HOME/.cargo/bin:$PATH"

(cd web && npm run build:wasm)

rm -rf "$out"
mkdir -p "$out"
cp -R web/dist-wasm/. "$out/"
echo "$(find "$out" -type f | wc -l) files, $(du -sh "$out" | cut -f1)"

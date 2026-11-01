#!/usr/bin/env bash
# Builds the in-browser editor and lays it out as a directory ready to upload:
#
#   scripts/package-editor.sh [out-dir]        (default: editor-bundle)
#
# The directory is a plain static site (relative URLs, so any path or its own
# subdomain works) with, next to it:
#   _headers               security + cache headers (Netlify / Cloudflare Pages)
#   security-headers.conf  the same security headers as nginx `add_header` lines
#   nginx-site.conf        an example server block that uses them
#   DEPLOY.md              what to configure, and how to check it
#   VERSION                the commit and engine version it was built from
# and every text/wasm file is also written pre-compressed (.br, .gz) for
# servers that serve those (nginx `brotli_static` / `gzip_static`).
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:-editor-bundle}"
export PATH="$HOME/.cargo/bin:$PATH"

(cd web && npm run build:wasm)

rm -rf "$out"
mkdir -p "$out"
cp -R web/dist-wasm/. "$out/"

cat > "$out/nginx-site.conf" <<'CONF'
# Example only: adjust server_name, certificate paths and root.
server {
    listen 443 ssl http2;
    server_name editor.meshfox.orofarne.net;
    ssl_certificate     /etc/letsencrypt/live/editor.meshfox.orofarne.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/editor.meshfox.orofarne.net/privkey.pem;

    root /var/www/editor.meshfox.orofarne.net;
    index index.html;

    # Pre-compressed files written next to the originals (.br needs the
    # ngx_brotli module; without it, remove brotli_static and gzip_static serves).
    gzip_static on;
    brotli_static on;

    include /etc/nginx/snippets/meshfox-editor-security-headers.conf;

    location = /index.html {
        add_header Cache-Control "no-cache" always;
        include /etc/nginx/snippets/meshfox-editor-security-headers.conf;
    }
    location /assets/ {
        # .wasm must be application/wasm (streaming compilation); recent nginx
        # maps it already, this covers the rest. Do not replace `types` here:
        # that would drop every other mapping.
        default_type application/wasm;
        add_header Cache-Control "public, max-age=31536000, immutable" always;
        include /etc/nginx/snippets/meshfox-editor-security-headers.conf;
    }
    location / {
        try_files $uri $uri/ =404;
    }
}
CONF

{
  echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- || echo ' (with uncommitted changes)')"
  echo "built:  $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out/VERSION"

cat > "$out/DEPLOY.md" <<'DOC'
# Deploying the meshfox editor

This directory is the whole site. Upload it as-is to a host that serves static
files, at the root of its **own origin** (e.g. `editor.meshfox.orofarne.net`):
drafts live in that origin's IndexedDB, so nothing else should be served from it.

1. Serve the files with the headers in `_headers` (Netlify / Cloudflare Pages
   read that file themselves), or on nginx copy `security-headers.conf` to
   `/etc/nginx/snippets/meshfox-editor-security-headers.conf` and start from
   `nginx-site.conf`.
2. `.wasm` must be sent as `application/wasm`.
3. Check, with `curl -sI https://<host>/`: `content-security-policy`,
   `x-content-type-options: nosniff`, `referrer-policy: no-referrer`,
   `cross-origin-opener-policy: same-origin`, `x-frame-options: DENY`;
   `curl -sI https://<host>/assets/<any>.js` shows `immutable`; `index.html`
   shows `no-cache`.
4. Open the site: the start page shows "Loading the engine…" and then lets you
   open a canvas. Back at the browser console there must be no
   "Content Security Policy" messages.

Updating: replace the directory's contents. Keep the previous `assets/` files
for a while — a tab opened before the update still names them.
DOC

# Pre-compressed copies. Already-compressed formats are left alone.
find "$out" -type f \( -name '*.js' -o -name '*.css' -o -name '*.wasm' -o -name '*.html' -o -name '*.svg' -o -name '*.json' -o -name '*.ico' \) |
  while read -r f; do
    brotli -q 11 -f -k "$f" -o "$f.br"
    gzip -9 -f -k -n "$f"
  done

du -sh "$out"
echo "wasm: $(ls -l "$out"/assets/*.wasm | awk '{print $5}') bytes, $(ls -l "$out"/assets/*.wasm.br | awk '{print $5}') brotli"

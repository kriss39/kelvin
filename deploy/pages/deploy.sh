#!/bin/sh
# Publishes https://kelvinscan.pages.dev pointing at a Kelvin server.
#   deploy/pages/deploy.sh [backend-url]    default: the current local tunnel (scripts/tunnel.sh url)
# Needs a Cloudflare account; the first run asks to log in (npx wrangler login).
set -e
cd "$(dirname "$0")"
BACKEND=${1:-$(../../scripts/tunnel.sh url)}
[ -n "$BACKEND" ] || { echo "no backend URL; start scripts/tunnel.sh or pass one" >&2; exit 1; }
./build.sh
cat > wrangler.toml <<TOML
name = "kelvinscan"
pages_build_output_dir = "dist"
compatibility_date = "2026-09-01"

[vars]
BACKEND = "$BACKEND"
TOML
npx --yes wrangler@4.141.0 pages project list 2>/dev/null | grep -q kelvinscan \
  || npx --yes wrangler@4.141.0 pages project create kelvinscan --production-branch main
npx --yes wrangler@4.141.0 pages deploy dist --project-name kelvinscan --branch main --commit-dirty=true
echo "https://kelvinscan.pages.dev -> $BACKEND"

#!/bin/sh
# Builds the static part of kelvinscan.pages.dev: the explorer, API docs and brand files.
# /v1/* and /healthz are forwarded to the Kelvin server by the functions in functions/.
set -e
cd "$(dirname "$0")"
ORIGIN=${ORIGIN:-https://kelvinscan.pages.dev}
rm -rf dist && mkdir -p dist/docs
sed "s|__ORIGIN__|$ORIGIN|g" ../../src/web/index.html > dist/index.html
cp ../../src/docs.html dist/docs/index.html
cp ../../src/openapi.yaml dist/openapi.yaml
cp ../../brand/og.png dist/og.png
cp ../../brand/kelvin-mark.svg dist/favicon.svg

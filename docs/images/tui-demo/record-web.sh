#!/bin/bash
# Records ../hall-web.gif: `hall web` in a light theme over the fictional
# repos, while hall-demo plays the daemon. Needs Node, Google Chrome (or
# CHROME=<path>) and gifski, e.g. `nix shell nixpkgs#gifski`; run
# `cargo build --bins --example hall-demo` first.
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
ACME="${ACME:-/tmp/acme}"
"$HERE/reseed.sh"
. "$HERE/demo-env.sh" "$ACME"
# puppeteer-core lives under target/, like capture.js's playwright-core.
DEPS="$HERE/../../../target/web-capture"
[ -d "$DEPS/node_modules/puppeteer-core" ] || npm i --prefix "$DEPS" --no-audit --no-fund --silent puppeteer-core@23.11.1
export NODE_PATH="$DEPS/node_modules"
FRAMES="$ACME/web-frames"
rm -rf "$FRAMES"
cd "$ACME/code/storefront"
hall web --port 0 --no-open --json >"$ACME/web.out" 2>&1 &
WEB=$!
trap 'kill $WEB 2>/dev/null' EXIT
until [ -s "$ACME/web.out" ]; do sleep 0.2; done
URL="$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$ACME/web.out")"
node "$HERE/web/record.js" "$URL" "$FRAMES" "$HERE/../../../target/debug/examples/hall-demo" "$ACME/code"
gifski --fps 10 --quality 90 --width 1440 -o "$HERE/../hall-web.gif" "$FRAMES"/steady/*.png

#!/bin/bash
# Seeds the fictional repos again under $ACME (default /tmp/acme).
set -e
ACME="${ACME:-/tmp/acme}"
HERE="$(cd "$(dirname "$0")" && pwd)"
rm -rf "$ACME"
mkdir -p "$ACME/home" "$ACME/hallhome" "$ACME/code"
. "$HERE/demo-env.sh" "$ACME"
"$HERE/../../../target/debug/examples/hall-demo" seed "$ACME/code"
cd "$ACME/code/storefront" && hall reindex --json >/dev/null

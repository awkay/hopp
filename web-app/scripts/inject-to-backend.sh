#!/usr/bin/env bash
# Copy the built web app into backend/web: index.html becomes web/web-app<suffix>.html
# (served for every SPA route) and the hashed JS/CSS/images go to web/static/react/,
# which the backend serves at /static/react/ (the Vite `base`).
#
#   scripts/inject-to-backend.sh [suffix]    e.g. "-debug"
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=static/react/assets
DEST=../backend/web
mkdir -p "$DEST/static/react"
cp "$SRC/index.html" "$DEST/web-app${1:-}.html"
find "$SRC" -maxdepth 1 -type f ! -name index.html -exec cp {} "$DEST/static/react/" \;

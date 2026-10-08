#!/usr/bin/env bash
#
# Install the last Dataico build (packaging/dataico/build-macos.sh) to /Applications:
# quit a running Hopp, replace /Applications/hopp.app, launch it. See DATAICO.md.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.dataico.hopp"
BUILT_APP="$REPO_ROOT/tauri/src-tauri/target/release/bundle/macos/hopp.app"
INSTALLED_APP="/Applications/hopp.app"

die() { printf '%s\n' "$@" >&2; exit 1; }
bundle_id() { /usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$1/Contents/Info.plist"; }

[[ "$(uname -s)" == Darwin ]] || die "Only macOS is supported for now."
[[ -d "$BUILT_APP" ]] || die "No build at $BUILT_APP; run packaging/dataico/build-macos.sh first."
[[ "$(bundle_id "$BUILT_APP")" == "$APP_ID" ]] \
  || die "$BUILT_APP is not a Dataico build (bundle ID $(bundle_id "$BUILT_APP"))."
# Both apps are named hopp.app; never overwrite the official one.
if [[ -d "$INSTALLED_APP" && "$(bundle_id "$INSTALLED_APP")" != "$APP_ID" ]]; then
  die "$INSTALLED_APP is $(bundle_id "$INSTALLED_APP"), not $APP_ID. Not replacing it." \
      "Remove it yourself first; it also conflicts with our hopp:// login redirect (see DATAICO.md)."
fi

running="$INSTALLED_APP/Contents/MacOS/"
if pgrep -f "$running" >/dev/null; then
  echo "Quitting Hopp (ends any active call)..."
  osascript -e "quit app id \"$APP_ID\"" >/dev/null 2>&1 || true
  for _ in {1..20}; do
    pgrep -f "$running" >/dev/null || break
    sleep 0.5
  done
  pkill -f "$running" || true
fi

rm -rf "$INSTALLED_APP.new"
ditto "$BUILT_APP" "$INSTALLED_APP.new"
rm -rf "$INSTALLED_APP"
mv "$INSTALLED_APP.new" "$INSTALLED_APP"
version="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$INSTALLED_APP/Contents/Info.plist")"
echo "Installed $INSTALLED_APP ($version, $(git -C "$REPO_ROOT" rev-parse --short HEAD))"

if codesign -dv "$INSTALLED_APP" 2>&1 | grep -q '^Signature=adhoc'; then
  echo "Unsigned build: Screen Recording and Accessibility grants from the previous build no longer apply."
  echo "If Hopp shows them enabled but can't capture or control, reset them and re-grant when prompted:"
  echo "  tccutil reset ScreenCapture $APP_ID && tccutil reset Accessibility $APP_ID"
fi

open "$INSTALLED_APP"

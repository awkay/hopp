#!/usr/bin/env bash
#
# Build the Dataico-branded Hopp desktop app for macOS.
#
#   packaging/dataico/build-macos.sh
#
# Output: dist/dataico/Hopp-Dataico-<version>-<shortsha>-<arch>.zip
#
# Signing / notarization is driven by Tauri's standard environment variables
# (they may also be put in packaging/dataico/.env, see .env.example):
#
#   APPLE_SIGNING_IDENTITY   "Developer ID Application: <Name> (<TEAMID>)"
#                            unset -> unsigned (ad-hoc) build
#   Notarization, preferred (App Store Connect API key):
#     APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH
#   Notarization, alternative (Apple ID + app-specific password):
#     APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
#
# Other knobs:
#   VITE_API_BASE_URL   server host, no scheme (default hopp.apps.dataico.world)
#   LK_CUSTOM_WEBRTC    optional prebuilt libwebrtc dir; otherwise webrtc-sys
#                       downloads its own copy (~hundreds of MB) on first build
#
# See DATAICO.md at the repo root for the full story.

set -euo pipefail
unset CDPATH   # a user CDPATH makes `cd` print paths and breaks $(cd ... && pwd)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OVERLAY_CONFIG="src-tauri/tauri.conf.dataico.json"   # relative to tauri/
DEFAULT_API_BASE_URL="hopp.apps.dataico.world"
REQUIRED_NODE_MAJOR=20

log()  { printf '[dataico-build] %s\n' "$*"; }
warn() { printf '[dataico-build] WARNING: %s\n' "$*" >&2; }
die()  { printf '[dataico-build] ERROR: %s\n' "$*" >&2; exit 1; }
banner() {
  printf '\n'
  printf '%s\n' "======================================================================"
  local line
  for line in "$@"; do printf '  %s\n' "$line"; done
  printf '%s\n' "======================================================================"
}

# ---------------------------------------------------------------------------
# Optional .env (git-ignored). Variables already set in the environment win.
# ---------------------------------------------------------------------------
ENV_FILE="$SCRIPT_DIR/.env"
if [[ -f "$ENV_FILE" ]]; then
  log "Loading $ENV_FILE (values already in the environment take precedence)"
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
    line="${line#export }"
    key="${line%%=*}"
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "Bad line in $ENV_FILE: $line"
    if [[ -z "${!key+x}" ]]; then
      eval "export $line"
    fi
  done < "$ENV_FILE"
fi

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------
[[ "$(uname -s)" == "Darwin" ]] || die "This script builds the macOS app and must run on macOS."

if ! xcode-select -p >/dev/null 2>&1 || ! xcrun --find clang >/dev/null 2>&1; then
  die "Xcode Command Line Tools not found. Install them with: xcode-select --install"
fi
for tool in codesign ditto /usr/libexec/PlistBuddy; do
  command -v "$tool" >/dev/null 2>&1 || die "Required tool '$tool' not found (it ships with macOS / Xcode CLT)."
done

# Rust: accept whatever is on PATH, otherwise try the usual install locations.
if ! command -v cargo >/dev/null 2>&1; then
  for dir in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin; do
    if [[ -x "$dir/cargo" ]]; then export PATH="$dir:$PATH"; break; fi
  done
fi
command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1 \
  || die "Rust not found. Install rustup (https://rustup.rs or 'brew install rustup && rustup-init'), then re-run."

HOST_TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
case "$HOST_TRIPLE" in
  aarch64-apple-darwin) ARCH_LABEL="arm64" ;;
  x86_64-apple-darwin)  ARCH_LABEL="x86_64"; warn "x86_64 builds are expected to work but have not been tested." ;;
  *) die "Unsupported Rust host triple '$HOST_TRIPLE'. Need aarch64-apple-darwin (Apple Silicon) or x86_64-apple-darwin." ;;
esac

# Node 20 (the repo's .nvmrc). Newer Node (e.g. 26) breaks the pinned yarn 4.9.2.
node_major() { "$1" -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0; }
NODE_BIN=""
if command -v node >/dev/null 2>&1 && [[ "$(node_major node)" == "$REQUIRED_NODE_MAJOR" ]]; then
  NODE_BIN="$(command -v node)"
fi
if [[ -z "$NODE_BIN" && -d "$HOME/.nvm/versions/node" ]]; then
  candidate="$(ls -d "$HOME/.nvm/versions/node/v${REQUIRED_NODE_MAJOR}."* 2>/dev/null | sort -V | tail -1 || true)"
  [[ -n "$candidate" && -x "$candidate/bin/node" ]] && NODE_BIN="$candidate/bin/node"
fi
if [[ -z "$NODE_BIN" ]]; then
  for dir in "/opt/homebrew/opt/node@${REQUIRED_NODE_MAJOR}/bin" "/usr/local/opt/node@${REQUIRED_NODE_MAJOR}/bin"; do
    [[ -x "$dir/node" ]] && { NODE_BIN="$dir/node"; break; }
  done
fi
[[ -n "$NODE_BIN" ]] || die "Node.js ${REQUIRED_NODE_MAJOR}.x not found (current: $(node -v 2>/dev/null || echo none)).
  Install it with nvm ('nvm install ${REQUIRED_NODE_MAJOR}') or Homebrew ('brew install node@${REQUIRED_NODE_MAJOR}').
  Newer Node versions break the repo's pinned yarn 4.9.2."
export PATH="$(dirname "$NODE_BIN"):$PATH"

YARN_JS="$(ls "$REPO_ROOT"/.yarn/releases/yarn-*.cjs 2>/dev/null | head -1 || true)"
[[ -n "$YARN_JS" ]] || die "Pinned yarn release not found under .yarn/releases. Is this a complete clone?"
yarn_run() { node "$YARN_JS" "$@"; }

# ---------------------------------------------------------------------------
# Build environment
# ---------------------------------------------------------------------------
export VITE_API_BASE_URL="${VITE_API_BASE_URL:-$DEFAULT_API_BASE_URL}"
export VITE_OS="macos"
export VITE_BOTTOM_ARROW="false"
# No upstream telemetry: empty Sentry DSNs, no sourcemap upload, no PostHog.
export SENTRY_DSN_RUST=""
export VITE_SENTRY_DSN_JS=""
export VITE_POSTHOG_API_KEY=""
export VITE_POSTHOG_HOST=""
unset SENTRY_AUTH_TOKEN
# Paths below assume the default per-crate target dirs (core's build.rs also hardcodes them).
unset CARGO_TARGET_DIR
# The updater is disabled by the overlay config (empty plugins.updater.endpoints),
# so no updater signing key is needed. Make sure a stray one doesn't change behavior.
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

if [[ -n "${LK_CUSTOM_WEBRTC:-}" ]]; then
  [[ -d "$LK_CUSTOM_WEBRTC" ]] || die "LK_CUSTOM_WEBRTC is set but '$LK_CUSTOM_WEBRTC' is not a directory."
  export LK_CUSTOM_WEBRTC
  log "Using prebuilt libwebrtc from LK_CUSTOM_WEBRTC=$LK_CUSTOM_WEBRTC"
else
  log "LK_CUSTOM_WEBRTC not set; webrtc-sys will download libwebrtc on the first build if not cached."
fi

# ---------------------------------------------------------------------------
# Signing mode + preflight (fail fast, before the long build)
# ---------------------------------------------------------------------------
SIGNED=0
NOTARIZE_MODE="none"
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  SIGNED=1
  if [[ -n "${APPLE_CERTIFICATE:-}" ]]; then
    log "APPLE_CERTIFICATE is set; Tauri will import it into a temporary keychain."
  elif ! security find-identity -v -p codesigning | grep -Fq "\"$APPLE_SIGNING_IDENTITY\""; then
    security find-identity -v -p codesigning >&2 || true
    die "Signing identity \"$APPLE_SIGNING_IDENTITY\" was not found among the valid code-signing identities above.
  Install the Developer ID Application certificate WITH its private key into your login keychain
  (see DATAICO.md), or set APPLE_SIGNING_IDENTITY to one of the names listed."
  fi
  [[ "$APPLE_SIGNING_IDENTITY" == Developer\ ID\ Application:* ]] \
    || warn "APPLE_SIGNING_IDENTITY is not a 'Developer ID Application' identity; notarization/Gatekeeper will reject other identity types."

  if [[ -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
    [[ -f "$APPLE_API_KEY_PATH" ]] || die "APPLE_API_KEY_PATH '$APPLE_API_KEY_PATH' does not exist."
    NOTARIZE_MODE="api-key"
    unset APPLE_ID APPLE_PASSWORD   # let Tauri pick the API key path unambiguously
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARIZE_MODE="apple-id"
  else
    warn "APPLE_SIGNING_IDENTITY is set but no complete notarization credentials were found."
    warn "The app will be signed but NOT notarized; Gatekeeper will still block it on other Macs."
    warn "Set APPLE_API_ISSUER + APPLE_API_KEY + APPLE_API_KEY_PATH (preferred) or APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID."
    unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  fi
else
  # Make sure Tauri doesn't see half a signing configuration.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH \
        APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
fi

VERSION="$(node -p "require('$REPO_ROOT/tauri/src-tauri/tauri.conf.json').version")"
SHORT_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo nogit)"
if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=no 2>/dev/null || true)" ]]; then
  SHORT_SHA="${SHORT_SHA}-dirty"
fi

log "Repo:        $REPO_ROOT"
log "Version:     $VERSION ($SHORT_SHA), target $HOST_TRIPLE"
log "Node:        $(node -v) ($NODE_BIN)"
log "Rust:        $(rustc -V)"
log "Server:      $VITE_API_BASE_URL"
if [[ "$SIGNED" == 1 ]]; then
  log "Signing:     $APPLE_SIGNING_IDENTITY (notarization: $NOTARIZE_MODE)"
else
  log "Signing:     NONE (ad-hoc) - APPLE_SIGNING_IDENTITY not set"
fi

# ---------------------------------------------------------------------------
# 1. JS dependencies
# ---------------------------------------------------------------------------
log "Step 1/4: yarn install"
(cd "$REPO_ROOT" && yarn_run install)

# ---------------------------------------------------------------------------
# 2. hopp_core sidecar (release)
# ---------------------------------------------------------------------------
# core/build.rs links the release binary straight to
# core/target/release/hopp_core-<host-triple>, which is exactly the name Tauri's
# externalBin ("../../core/target/release/hopp_core") expects for the host target.
log "Step 2/4: cargo build --release (core/)"
(cd "$REPO_ROOT/core" && cargo build --release)
SIDECAR_SRC="$REPO_ROOT/core/target/release/hopp_core-$HOST_TRIPLE"
[[ -x "$SIDECAR_SRC" ]] || die "Expected sidecar binary $SIDECAR_SRC was not produced by the core build."

# ---------------------------------------------------------------------------
# 3. Tauri app bundle
# ---------------------------------------------------------------------------
log "Step 3/4: tauri build --bundles app --config $OVERLAY_CONFIG"
BUNDLE_DIR="$REPO_ROOT/tauri/src-tauri/target/release/bundle/macos"
rm -rf "$BUNDLE_DIR"
(cd "$REPO_ROOT/tauri" && yarn_run tauri build --bundles app --config "$OVERLAY_CONFIG")

APP_PATH="$(ls -d "$BUNDLE_DIR"/*.app 2>/dev/null | head -1 || true)"
[[ -n "$APP_PATH" && -d "$APP_PATH" ]] || die "No .app bundle found in $BUNDLE_DIR after the Tauri build."
MAIN_EXE="$APP_PATH/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$APP_PATH/Contents/Info.plist")"
SIDECAR="$APP_PATH/Contents/MacOS/hopp_core"
ENTITLEMENTS="$REPO_ROOT/tauri/src-tauri/entitlements.plist"

# ---------------------------------------------------------------------------
# 4. Sign / verify
# ---------------------------------------------------------------------------
log "Step 4/4: signing and verification"

sig_field() { codesign -dv --verbose=4 "$1" 2>&1 | awk -F= -v k="$2" '$1==k {print substr($0, length(k)+2); exit}'; }
# Capture first: `grep -q` exits on the first match, codesign then dies of SIGPIPE and pipefail fails the check.
has_runtime() { local info; info="$(codesign -dv --verbose=4 "$1" 2>&1)"; grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]*\(.*runtime' <<<"$info"; }
authority() { codesign -dv --verbose=4 "$1" 2>&1 | awk -F= '$1=="Authority" {print $2; exit}'; }

notarize_and_staple() {
  local zip="$REPO_ROOT/dist/dataico/.notarize-$$.zip"
  mkdir -p "$(dirname "$zip")"
  ditto -c -k --keepParent "$APP_PATH" "$zip"
  if [[ "$NOTARIZE_MODE" == "api-key" ]]; then
    xcrun notarytool submit "$zip" --wait \
      --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER"
  else
    xcrun notarytool submit "$zip" --wait \
      --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID"
  fi
  rm -f "$zip"
  xcrun stapler staple "$APP_PATH"
}

[[ -x "$SIDECAR" ]] || die "Sidecar hopp_core missing from $APP_PATH/Contents/MacOS."

# hopp_core only links system libraries (/usr/lib, /System, @rpath=/usr/lib/swift),
# so it does not need com.apple.security.cs.disable-library-validation. Guard it:
nonsystem_libs="$(otool -L "$SIDECAR" | tail -n +2 | awk '{print $1}' \
  | grep -Ev '^(/usr/lib/|/System/Library/|@rpath/libswift)' || true)"
if [[ -n "$nonsystem_libs" ]]; then
  warn "hopp_core links non-system libraries; hardened runtime may refuse to load them:"
  printf '    %s\n' $nonsystem_libs >&2
fi

if [[ "$SIGNED" == 0 ]]; then
  # Tauri leaves only the linker's ad-hoc signatures when no identity is set, which
  # fails 'codesign --verify --strict'. Seal the bundle consistently (inside-out).
  codesign --force --sign - --entitlements "$ENTITLEMENTS" "$SIDECAR"
  codesign --force --sign - --entitlements "$ENTITLEMENTS" "$APP_PATH"
  codesign --verify --deep --strict --verbose=2 "$APP_PATH" || die "Ad-hoc signature verification failed."
else
  APP_TEAM="$(sig_field "$MAIN_EXE" TeamIdentifier)"
  SIDECAR_TEAM="$(sig_field "$SIDECAR" TeamIdentifier)"
  if [[ "$(authority "$SIDECAR")" != "$APPLE_SIGNING_IDENTITY" && -z "${APPLE_CERTIFICATE:-}" ]] \
     || [[ "$SIDECAR_TEAM" != "$APP_TEAM" ]] || ! has_runtime "$SIDECAR" || ! has_runtime "$MAIN_EXE"; then
    warn "Tauri did not sign hopp_core / the app with the identity + hardened runtime; re-signing."
    codesign --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" \
      --sign "$APPLE_SIGNING_IDENTITY" "$SIDECAR"
    codesign --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" \
      --sign "$APPLE_SIGNING_IDENTITY" "$APP_PATH"
    if [[ "$NOTARIZE_MODE" != "none" ]]; then
      log "Re-signed after Tauri's notarization; notarizing again with notarytool."
      notarize_and_staple
    fi
    APP_TEAM="$(sig_field "$MAIN_EXE" TeamIdentifier)"
    SIDECAR_TEAM="$(sig_field "$SIDECAR" TeamIdentifier)"
  fi

  codesign --verify --deep --strict --verbose=2 "$APP_PATH" || die "codesign --verify --deep --strict failed."
  [[ "$SIDECAR_TEAM" == "$APP_TEAM" && "$APP_TEAM" != "not set" ]] \
    || die "Team ID mismatch: app=$APP_TEAM hopp_core=$SIDECAR_TEAM"
  has_runtime "$SIDECAR" || die "hopp_core is not signed with the hardened runtime."
  log "hopp_core signed by '$(authority "$SIDECAR")' (team $SIDECAR_TEAM) with hardened runtime."

  if [[ "$NOTARIZE_MODE" != "none" ]]; then
    if ! xcrun stapler validate "$APP_PATH"; then
      log "No stapled ticket found; stapling."
      xcrun stapler staple "$APP_PATH"
      xcrun stapler validate "$APP_PATH" || die "stapler validate failed after stapling."
    fi
    spctl -a -vvv -t exec "$APP_PATH" || die "Gatekeeper (spctl) rejected the notarized app."
  else
    spctl -a -vvv -t exec "$APP_PATH" \
      || warn "spctl rejects the app as expected for a signed-but-not-notarized build."
  fi
fi

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$APP_PATH/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.dataico.hopp" ]] || die "Unexpected CFBundleIdentifier '$BUNDLE_ID' (expected com.dataico.hopp)."
grep -rqF "$VITE_API_BASE_URL" "$REPO_ROOT/tauri/dist/assets" \
  || die "Built frontend (tauri/dist) does not contain '$VITE_API_BASE_URL'."

# ---------------------------------------------------------------------------
# Package
# ---------------------------------------------------------------------------
OUT_DIR="$REPO_ROOT/dist/dataico"
ZIP_PATH="$OUT_DIR/Hopp-Dataico-${VERSION}-${SHORT_SHA}-${ARCH_LABEL}.zip"
mkdir -p "$OUT_DIR"
rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"

if [[ "$SIGNED" == 1 && "$NOTARIZE_MODE" != "none" ]]; then
  banner "DONE: signed + notarized build" \
         "App:  $APP_PATH" \
         "Zip:  $ZIP_PATH" \
         "Server: $VITE_API_BASE_URL   Bundle ID: $BUNDLE_ID"
elif [[ "$SIGNED" == 1 ]]; then
  banner "DONE: SIGNED BUT NOT NOTARIZED" \
         "Gatekeeper will block this app on other Macs until it is notarized." \
         "App:  $APP_PATH" \
         "Zip:  $ZIP_PATH" \
         "Server: $VITE_API_BASE_URL   Bundle ID: $BUNDLE_ID"
else
  banner "DONE: UNSIGNED (AD-HOC) BUILD - NOT FOR DISTRIBUTION" \
         "Fine on this Mac. On any other Mac, Gatekeeper reports it as damaged unless the" \
         "recipient runs: xattr -dr com.apple.quarantine /Applications/<app>.app" \
         "macOS privacy grants (Screen Recording, Accessibility) reset on every ad-hoc rebuild." \
         "Set APPLE_SIGNING_IDENTITY (+ notarization creds) for a distributable build." \
         "App:  $APP_PATH" \
         "Zip:  $ZIP_PATH" \
         "Server: $VITE_API_BASE_URL   Bundle ID: $BUNDLE_ID"
fi

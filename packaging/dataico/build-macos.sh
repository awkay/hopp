#!/usr/bin/env bash
#
# Build the Dataico-branded Hopp desktop app for macOS.
#
#   packaging/dataico/build-macos.sh
#
# Output: dist/dataico/Hopp-Dataico-<version>-<shortsha>-<arch>.zip, and with the
# updater on also hopp_<arch>.app.tar.gz, its .sig and latest.json (the update files).
# Last, dist/dataico/build-info.json records what was built (commit, server, feed, team,
# notarization); release.sh reuses a build only if it matches the release.
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
# In-app updater: on only for notarized builds with the updater key, and a non-empty
# pubkey in tauri.conf.dataico-updater.json. Everything else checks no feed.
#   TAURI_SIGNING_PRIVATE_KEY           updater private key (the file's contents or its path)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  its password
#   DATAICO_RELEASE_NOTES               file with the release notes for latest.json
#   DATAICO_UPDATER_ENDPOINT            feed URL instead of our latest release, for testing;
#                                       latest.json then points to the tarball next to it
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
LOG_PREFIX="dataico-build"
# shellcheck source=SCRIPTDIR/lib.sh
source "$SCRIPT_DIR/lib.sh"
OVERLAY_CONFIG="src-tauri/tauri.conf.dataico.json"   # relative to tauri/
UPDATER_CONFIG="src-tauri/tauri.conf.dataico-updater.json"
REQUIRED_NODE_MAJOR=20

# From here until build-info.json is written again, dist/dataico holds no finished build.
rm -f "$BUILD_INFO"

# Temp files live in $WORK. If the build fails or is interrupted, the dist/dataico files it has
# started writing ($OUTPUTS) go too: a partial set, with nothing to vouch for it.
WORK="$(mktemp -d)"
OUTPUTS=()
cleanup() {
  local status=$?
  rm -rf "$WORK"
  if [[ "$status" != 0 ]] && (( ${#OUTPUTS[@]} )); then rm -f "${OUTPUTS[@]}"; fi
}
trap cleanup EXIT
# Exit explicitly so cleanup sees a failure: on Ctrl-C bash 5 runs the EXIT trap with $? = 0.
trap 'exit 130' INT
trap 'exit 143' TERM

# Optional .env (git-ignored). Variables already set in the environment win.
load_env export

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
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
  die "Rust not found. Install rustup (https://rustup.rs or 'brew install rustup && rustup-init'), then re-run."
fi

HOST_TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
artifact_names "$HOST_TRIPLE" \
  || die "Unsupported Rust host triple '$HOST_TRIPLE'. Need aarch64-apple-darwin (Apple Silicon) or x86_64-apple-darwin."
[[ "$HOST_TRIPLE" != x86_64-apple-darwin ]] || warn "x86_64 builds are expected to work but have not been tested."

# Node 20 (the repo's .nvmrc). Newer Node (e.g. 26) breaks the pinned yarn 4.9.2.
node_major() { "$1" -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0; }
NODE_BIN=""
if command -v node >/dev/null 2>&1 && [[ "$(node_major node)" == "$REQUIRED_NODE_MAJOR" ]]; then
  NODE_BIN="$(command -v node)"
fi
if [[ -z "$NODE_BIN" && -d "$HOME/.nvm/versions/node" ]]; then
  candidate="$(printf '%s\n' "$HOME/.nvm/versions/node/v${REQUIRED_NODE_MAJOR}."* | sort -V | tail -1)"
  [[ -n "$candidate" && -x "$candidate/bin/node" ]] && NODE_BIN="$candidate/bin/node"
fi
MISE_NODE_DIR="${MISE_DATA_DIR:-$HOME/.local/share/mise}/installs/node"
if [[ -z "$NODE_BIN" && -d "$MISE_NODE_DIR" ]]; then
  candidate="$(printf '%s\n' "$MISE_NODE_DIR/${REQUIRED_NODE_MAJOR}."* | sort -V | tail -1)"
  [[ -n "$candidate" && -x "$candidate/bin/node" ]] && NODE_BIN="$candidate/bin/node"
fi
if [[ -z "$NODE_BIN" ]]; then
  for dir in "/opt/homebrew/opt/node@${REQUIRED_NODE_MAJOR}/bin" "/usr/local/opt/node@${REQUIRED_NODE_MAJOR}/bin"; do
    [[ -x "$dir/node" ]] && { NODE_BIN="$dir/node"; break; }
  done
fi
[[ -n "$NODE_BIN" ]] || die "Node.js ${REQUIRED_NODE_MAJOR}.x not found (current: $(node -v 2>/dev/null || echo none)).
  Install it with nvm ('nvm install ${REQUIRED_NODE_MAJOR}'), mise ('mise install node@${REQUIRED_NODE_MAJOR}')
  or Homebrew ('brew install node@${REQUIRED_NODE_MAJOR}').
  Newer Node versions break the repo's pinned yarn 4.9.2."
NODE_DIR="$(dirname "$NODE_BIN")"
export PATH="$NODE_DIR:$PATH"

YARN_JS="$(printf '%s\n' "$REPO_ROOT"/.yarn/releases/yarn-*.cjs | head -1)"
[[ -f "$YARN_JS" ]] || die "Pinned yarn release not found under .yarn/releases. Is this a complete clone?"
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
# Only this script signs with the updater key (see "Update files" below); Tauri's own
# updater artifacts stay off, so keep the key out of tauri build's environment.
UPDATER_KEY="${TAURI_SIGNING_PRIVATE_KEY:-}"
UPDATER_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD TAURI_SIGNING_PRIVATE_KEY_PATH

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
  elif ! identity_in_keychain "$APPLE_SIGNING_IDENTITY"; then
    security find-identity -v -p codesigning >&2 || true
    die "Signing identity \"$APPLE_SIGNING_IDENTITY\" was not found among the valid code-signing identities above.
  Install the Developer ID Application certificate WITH its private key into your login keychain
  (see DATAICO.md), or set APPLE_SIGNING_IDENTITY to one of the names listed."
  fi
  [[ "$APPLE_SIGNING_IDENTITY" == Developer\ ID\ Application:* ]] \
    || warn "APPLE_SIGNING_IDENTITY is not a 'Developer ID Application' identity; notarization/Gatekeeper will reject other identity types."

  NOTARIZE_MODE="$(notarize_mode)"
  case "$NOTARIZE_MODE" in
    api-key)
      [[ -f "$APPLE_API_KEY_PATH" ]] || die "APPLE_API_KEY_PATH '$APPLE_API_KEY_PATH' does not exist."
      unset APPLE_ID APPLE_PASSWORD   # let Tauri pick the API key path unambiguously
      ;;
    none)
      warn "APPLE_SIGNING_IDENTITY is set but no complete notarization credentials were found."
      warn "The app will be signed but NOT notarized; Gatekeeper will still block it on other Macs."
      warn "Set $NOTARIZE_CREDENTIALS."
      unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
      ;;
  esac
else
  # Make sure Tauri doesn't see half a signing configuration.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH \
        APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
fi

# ---------------------------------------------------------------------------
# In-app updater. Builds without it keep the empty endpoint list from
# $OVERLAY_CONFIG, so the app never fetches a feed.
# ---------------------------------------------------------------------------
updater_conf() { node -p "require(process.argv[1]).plugins.updater.$1 || ''" "$REPO_ROOT/tauri/$UPDATER_CONFIG"; }
UPDATER_PUBKEY="$(updater_conf pubkey)"
UPDATER_ENDPOINT="${DATAICO_UPDATER_ENDPOINT:-$(updater_conf 'endpoints[0]')}"
UPDATER=0
TAURI_CONFIGS=(--config "$OVERLAY_CONFIG")
if [[ "$NOTARIZE_MODE" == "none" ]]; then
  UPDATER_OFF_REASON="not notarized"
elif [[ -z "$UPDATER_KEY" ]]; then
  UPDATER_OFF_REASON="TAURI_SIGNING_PRIVATE_KEY is not set"
elif [[ -z "$UPDATER_PUBKEY" ]]; then
  UPDATER_OFF_REASON="plugins.updater.pubkey is empty in tauri/$UPDATER_CONFIG"
else
  UPDATER=1
  # --config may be repeated (tauri-cli >= 2.4); later files win, arrays are replaced.
  TAURI_CONFIGS+=(--config "$UPDATER_CONFIG")
  if [[ -n "${DATAICO_UPDATER_ENDPOINT:-}" ]]; then
    TAURI_CONFIGS+=(--config "$(node -e 'console.log(JSON.stringify({plugins: {updater: {endpoints: [process.argv[1]]}}}))' "$DATAICO_UPDATER_ENDPOINT")")
  fi
  if [[ -n "${DATAICO_RELEASE_NOTES:-}" ]]; then
    [[ -f "$DATAICO_RELEASE_NOTES" ]] || die "DATAICO_RELEASE_NOTES '$DATAICO_RELEASE_NOTES' does not exist."
  else
    warn "DATAICO_RELEASE_NOTES is not set; latest.json gets empty notes."
  fi
fi
if [[ "$UPDATER" == 0 && "$NOTARIZE_MODE" != "none" ]]; then
  warn "THE UPDATER IS OFF ($UPDATER_OFF_REASON)."
  warn "This notarized build will never offer updates, and no update files are made."
fi
[[ "$UPDATER" == 1 || -z "${DATAICO_UPDATER_ENDPOINT:-}" ]] \
  || warn "DATAICO_UPDATER_ENDPOINT is ignored: the updater is off."

# `tauri signer sign` takes the key's contents only (a path fails to decode), unlike `tauri
# build`. The secrets go through the environment, not argv, to keep them out of `ps`.
updater_sign() (
  TAURI_SIGNING_PRIVATE_KEY="$UPDATER_KEY"
  if [[ -f "$UPDATER_KEY" ]]; then TAURI_SIGNING_PRIVATE_KEY="$(<"$UPDATER_KEY")"; fi
  export TAURI_SIGNING_PRIVATE_KEY
  if [[ -n "$UPDATER_KEY_PASSWORD" ]]; then
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$UPDATER_KEY_PASSWORD"
  fi
  cd "$REPO_ROOT/tauri" && yarn_run tauri signer sign "$1" >/dev/null
)

# Checks $1.sig against the pubkey built into the app, as the updater does before installing:
# minisign key id, then Ed25519 over the file's BLAKE2b-512 hash ("ED") or the file ("Ed").
verify_update_sig() {
  node -e '
    const fs = require("fs"), crypto = require("crypto");
    const [pubkey, file] = process.argv.slice(1);
    const line2 = (b64) => Buffer.from(Buffer.from(b64, "base64").toString().split("\n")[1], "base64");
    const pub = line2(pubkey), sig = line2(fs.readFileSync(file + ".sig", "utf8"));
    if (!sig.subarray(2, 10).equals(pub.subarray(2, 10))) {
      console.error("signed by a different key than the pubkey in the updater config"); process.exit(1);
    }
    const der = Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pub.subarray(10)]);
    const key = crypto.createPublicKey({ key: der, format: "der", type: "spki" });
    let data = fs.readFileSync(file);
    if (sig.subarray(0, 2).toString() === "ED") data = crypto.createHash("blake2b512").update(data).digest();
    if (!crypto.verify(null, data, key, sig.subarray(10, 74))) { console.error("bad signature"); process.exit(1); }
  ' "$UPDATER_PUBKEY" "$1"
}

VERSION="$(node -p "require('$REPO_ROOT/tauri/src-tauri/tauri.conf.json').version")"
GIT_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || true)"
SHORT_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo nogit)"
DIRTY=0
if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=no 2>/dev/null || true)" ]]; then
  DIRTY=1
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
if [[ "$UPDATER" == 1 ]]; then
  log "Updater:     on, feed $UPDATER_ENDPOINT"
else
  log "Updater:     off ($UPDATER_OFF_REASON)"
fi

# ---------------------------------------------------------------------------
# 1. JS dependencies
# ---------------------------------------------------------------------------
log "Step 1/4: yarn install"
(cd "$REPO_ROOT" && yarn_run install)

# The signer comes with the JS dependencies. A wrong key or password would otherwise only
# show after the long build, and a key that doesn't match the pubkey would ship updates
# that every installed app rejects.
if [[ "$UPDATER" == 1 ]]; then
  log "Checking the updater key against the pubkey in tauri/$UPDATER_CONFIG"
  probe="$WORK/updater-key-check"
  echo "updater key check" > "$probe"
  updater_sign "$probe" || die "Could not sign with TAURI_SIGNING_PRIVATE_KEY (wrong key or TAURI_SIGNING_PRIVATE_KEY_PASSWORD?)."
  verify_update_sig "$probe" || die "TAURI_SIGNING_PRIVATE_KEY does not match the pubkey in tauri/$UPDATER_CONFIG."
fi

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
log "Step 3/4: tauri build --bundles app ${TAURI_CONFIGS[*]}"
BUNDLE_DIR="$REPO_ROOT/tauri/src-tauri/target/release/bundle/macos"
rm -rf "$BUNDLE_DIR"
(cd "$REPO_ROOT/tauri" && yarn_run tauri build --bundles app "${TAURI_CONFIGS[@]}")

APP_PATH="$(printf '%s\n' "$BUNDLE_DIR"/*.app | head -1)"
[[ -n "$APP_PATH" && -d "$APP_PATH" ]] || die "No .app bundle found in $BUNDLE_DIR after the Tauri build."
MAIN_EXE="$APP_PATH/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$APP_PATH/Contents/Info.plist")"
SIDECAR="$APP_PATH/Contents/MacOS/hopp_core"
ENTITLEMENTS="$REPO_ROOT/tauri/src-tauri/entitlements.plist"

# ---------------------------------------------------------------------------
# 4. Sign / verify
# ---------------------------------------------------------------------------
log "Step 4/4: signing and verification"
TEAM_ID=""     # for build-info.json, once verified
NOTARIZED=0

sig_field() { codesign -dv --verbose=4 "$1" 2>&1 | awk -F= -v k="$2" '$1==k {print substr($0, length(k)+2); exit}'; }
# Capture first: `grep -q` exits on the first match, codesign then dies of SIGPIPE and pipefail fails the check.
has_runtime() { local info; info="$(codesign -dv --verbose=4 "$1" 2>&1)"; grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]*\(.*runtime' <<<"$info"; }
authority() { codesign -dv --verbose=4 "$1" 2>&1 | awk -F= '$1=="Authority" {print $2; exit}'; }

notarize_and_staple() {
  local zip="$WORK/notarize.zip"
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
  while IFS= read -r lib; do printf '    %s\n' "$lib" >&2; done <<<"$nonsystem_libs"
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
  TEAM_ID="$APP_TEAM"

  if [[ "$NOTARIZE_MODE" != "none" ]]; then
    if ! xcrun stapler validate "$APP_PATH"; then
      log "No stapled ticket found; stapling."
      xcrun stapler staple "$APP_PATH"
      xcrun stapler validate "$APP_PATH" || die "stapler validate failed after stapling."
    fi
    spctl -a -vvv -t exec "$APP_PATH" || die "Gatekeeper (spctl) rejected the notarized app."
    NOTARIZED=1
  else
    spctl -a -vvv -t exec "$APP_PATH" \
      || warn "spctl rejects the app as expected for a signed-but-not-notarized build."
  fi
fi

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$APP_PATH/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.dataico.hopp" ]] || die "Unexpected CFBundleIdentifier '$BUNDLE_ID' (expected com.dataico.hopp)."
grep -rqF "$VITE_API_BASE_URL" "$REPO_ROOT/tauri/dist/assets" \
  || die "Built frontend (tauri/dist) does not contain '$VITE_API_BASE_URL'."
# The merged Tauri config is compiled into the main binary as plain strings.
if [[ "$UPDATER" == 1 ]]; then
  for expected in "$UPDATER_ENDPOINT" "$UPDATER_PUBKEY"; do
    LC_ALL=C grep -qaF "$expected" "$MAIN_EXE" \
      || die "The app binary lacks '$expected'; the updater --config merge did not apply."
  done
elif LC_ALL=C grep -qaF "$FEED_PATH" "$MAIN_EXE"; then
  die "The updater is off, yet the app binary contains an update feed URL."
fi

# ---------------------------------------------------------------------------
# Package
# ---------------------------------------------------------------------------
ZIP_PATH="$OUT_DIR/$(zip_name "$VERSION" "$SHORT_SHA")"
TARBALL="$OUT_DIR/$TARBALL_NAME"
FEED="$OUT_DIR/$FEED_NAME"
BUILD_INFO_TMP="$BUILD_INFO.tmp"
mkdir -p "$OUT_DIR"
# Drop the last build's files. Only build-info.json, written at the very end, tells release.sh
# that the files next to it are one finished build.
rm -f "$ZIP_PATH" "$TARBALL" "$TARBALL.sig" "$FEED" "$BUILD_INFO_TMP"
OUTPUTS=("$ZIP_PATH" "$TARBALL" "$TARBALL.sig" "$FEED" "$BUILD_INFO_TMP")

# Update files. Made here, not by Tauri (createUpdaterArtifacts stays false): step 4 may
# re-sign and re-notarize after Tauri, so Tauri's tarball could hold an app without the
# final signature or stapled ticket.
if [[ "$UPDATER" == 1 ]]; then
  log "Update files: $(basename "$TARBALL"), .sig, latest.json"
  # The updater drops each entry's first path component and installs the rest as the app, so
  # the tarball must hold only <name>.app: no ._ AppleDouble files (COPYFILE_DISABLE). The
  # updater also ignores xattrs; --no-xattrs makes the check below see what it installs.
  COPYFILE_DISABLE=1 tar --no-xattrs -czf "$TARBALL" -C "$(dirname "$APP_PATH")" "$(basename "$APP_PATH")"
  check_dir="$WORK/tarball-check"
  mkdir "$check_dir"
  tar -xzf "$TARBALL" -C "$check_dir"
  codesign --verify --deep --strict "$check_dir/$(basename "$APP_PATH")" \
    || die "The app in $(basename "$TARBALL") fails codesign --verify."
  xcrun stapler validate "$check_dir/$(basename "$APP_PATH")" \
    || die "The app in $(basename "$TARBALL") has no valid stapled ticket."
  rm -rf "$check_dir"

  updater_sign "$TARBALL" || die "Signing $(basename "$TARBALL") failed."
  verify_update_sig "$TARBALL" || die "The signature of $(basename "$TARBALL") does not verify."

  if [[ -n "${DATAICO_UPDATER_ENDPOINT:-}" ]]; then
    DOWNLOAD_URL="${DATAICO_UPDATER_ENDPOINT%/*}/$(basename "$TARBALL")"
  else
    DOWNLOAD_URL="$(release_download_url "$VERSION" "$TARBALL_NAME")"
  fi
  node -e '
    const fs = require("fs");
    const [version, notesFile, pubDate, platform, sigFile, url] = process.argv.slice(1);
    const notes = notesFile ? fs.readFileSync(notesFile, "utf8").trim() : "";
    const signature = fs.readFileSync(sigFile, "utf8").trim();
    console.log(JSON.stringify({ version, notes, pub_date: pubDate, platforms: { [platform]: { signature, url } } }, null, 2));
  ' "$VERSION" "${DATAICO_RELEASE_NOTES:-}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$UPDATER_PLATFORM" \
    "$TARBALL.sig" "$DOWNLOAD_URL" > "$FEED"
fi

ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"

# What this build is, now that every check has passed. release.sh publishes the files above
# without building again only if each field matches the release (lib.sh, build_info_mismatch).
# Written whole, then renamed, so a half-written file never exists under the real name.
INFO_ENDPOINT=""
INFO_NOTES=""
if [[ "$UPDATER" == 1 ]]; then
  INFO_ENDPOINT="$UPDATER_ENDPOINT"
  INFO_NOTES="${DATAICO_RELEASE_NOTES:-}"
fi
node -e '
  const fs = require("fs"), crypto = require("crypto");
  const [version, gitSha, dirty, hostTriple, apiBaseUrl, updater, endpoint, notesFile, teamId, notarized] =
    process.argv.slice(1);
  // Of the bytes latest.json took its notes from, so release.sh can tell they are its notes.
  const notesSha256 = notesFile ? crypto.createHash("sha256").update(fs.readFileSync(notesFile)).digest("hex") : "";
  console.log(JSON.stringify({
    version, git_sha: gitSha, dirty: dirty === "1", host_triple: hostTriple, api_base_url: apiBaseUrl,
    updater: updater === "1", updater_endpoint: endpoint, release_notes_sha256: notesSha256,
    team_id: teamId, notarized: notarized === "1", built_at: new Date().toISOString(),
  }, null, 2));
' "$VERSION" "$GIT_SHA" "$DIRTY" "$HOST_TRIPLE" "$VITE_API_BASE_URL" "$UPDATER" "$INFO_ENDPOINT" \
  "$INFO_NOTES" "$TEAM_ID" "$NOTARIZED" > "$BUILD_INFO_TMP"
mv -f "$BUILD_INFO_TMP" "$BUILD_INFO"
OUTPUTS=()   # finished: nothing left to clean up

if [[ "$UPDATER" == 1 ]]; then
  UPDATER_LINE="Updater: on, feed $UPDATER_ENDPOINT. Update files next to the zip."
else
  UPDATER_LINE="Updater: OFF ($UPDATER_OFF_REASON)"
fi
if [[ "$SIGNED" == 1 && "$NOTARIZE_MODE" != "none" ]]; then
  banner "DONE: signed + notarized build" \
         "App:  $APP_PATH" \
         "Zip:  $ZIP_PATH" \
         "$UPDATER_LINE" \
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

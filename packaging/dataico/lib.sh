# shellcheck shell=bash
#
# Shared by build-macos.sh and release.sh, which source it after `set -euo pipefail` and
# `unset CDPATH`, with LOG_PREFIX set. Not meant to be run.
#
# The two scripts are a contract: release.sh uploads the files build-macos.sh writes, and reuses
# a build only if its build-info.json matches the release. So the names they share live here.

# The constants are used by the scripts that source this file.
# shellcheck disable=SC2034
LOG_PREFIX="${LOG_PREFIX:-dataico}"
DATAICO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$DATAICO_DIR/../.." && pwd)"
ENV_FILE="$DATAICO_DIR/.env"
OUT_DIR="$REPO_ROOT/dist/dataico"
# What build-macos.sh built: deleted when a build starts, written last once every check passed.
BUILD_INFO="$OUT_DIR/build-info.json"

REPO_SLUG="awkay/hopp"
RELEASES_URL="https://github.com/$REPO_SLUG/releases"
FEED_PATH="releases/latest/download/latest.json"   # an update feed, in any GitHub repo
FEED_URL="https://github.com/$REPO_SLUG/$FEED_PATH"
FEED_NAME="latest.json"
TAG_PREFIX="dataico-v"
DEFAULT_API_BASE_URL="hopp.apps.dataico.world"
# x.y.z without leading zeros: Tauri's semver parser rejects 1.0.035, which `sort -V` accepts.
VERSION_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
# Only build-macos.sh may hold these: see load_env.
SECRET_VARS=(TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD APPLE_PASSWORD
             APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD)
NOTARIZE_CREDENTIALS="APPLE_API_ISSUER + APPLE_API_KEY + APPLE_API_KEY_PATH (preferred), or APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID"

log()  { printf '[%s] %s\n' "$LOG_PREFIX" "$*"; }
warn() { printf '[%s] WARNING: %s\n' "$LOG_PREFIX" "$*" >&2; }
die()  { printf '[%s] ERROR: %s\n' "$LOG_PREFIX" "$*" >&2; exit 1; }
banner() {
  printf '\n'
  printf '%s\n' "======================================================================"
  local line
  for line in "$@"; do printf '  %s\n' "$line"; done
  printf '%s\n' "======================================================================"
}

# ---------------------------------------------------------------------------
# packaging/dataico/.env (git-ignored), read with the shell's quoting rules. Variables that are
# already set win over the file.
#   load_env export  export every value (build-macos.sh: tauri build and notarytool read them)
#   load_env check   plain shell variables, which no child process sees; of the SECRET_VARS,
#                    only note that they are non-empty, for has_value (release.sh: it only
#                    checks them, and the build-macos.sh it runs reads .env itself)
# ---------------------------------------------------------------------------
ENV_SECRETS=" "
is_secret() { local s; for s in "${SECRET_VARS[@]}"; do [[ "$1" != "$s" ]] || return 0; done; return 1; }
# Whether variable $1 is non-empty, or is a secret that .env sets (load_env check).
has_value() { [[ -n "${!1:-}" || "$ENV_SECRETS" == *" $1 "* ]]; }

load_env() {
  local mode="$1" line key value n=0
  [[ -f "$ENV_FILE" ]] || return 0
  [[ "$mode" == check ]] || log "Loading $ENV_FILE (values already in the environment take precedence)"
  while IFS= read -r line || [[ -n "$line" ]]; do
    n=$((n + 1))
    [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
    line="${line#export }"
    key="${line%%=*}"
    # The line number only: the line may hold a secret.
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "Line $n of $ENV_FILE is not NAME=value."
    [[ -z "${!key+x}" ]] || continue
    if [[ "$mode" == export ]]; then
      eval "export $line"
    elif is_secret "$key"; then
      # Evaluated as `export` would, in a subshell that only says whether the value is empty.
      value="$(eval "export $line" && if [[ -n "${!key-}" ]]; then echo set; fi)" \
        || die "Could not read line $n of $ENV_FILE."
      [[ -z "$value" ]] || ENV_SECRETS+="$key "
    else
      value="$(eval "export $line" && printf '%s.' "${!key-}")" \
        || die "Could not read line $n of $ENV_FILE."
      printf -v "$key" '%s' "${value%.}"
    fi
  done < "$ENV_FILE"
}

# ---------------------------------------------------------------------------
# Signing and notarization
# ---------------------------------------------------------------------------
# Whether code-signing identity $1 is in the keychain. Captured first: with pipefail, `grep -q`
# exiting early can kill the writer with SIGPIPE and fail the check.
identity_in_keychain() {
  local ids
  ids="$(security find-identity -v -p codesigning)" || return 1
  grep -Fq "\"$1\"" <<<"$ids"
}

# The Team ID that ends a Developer ID identity, "Developer ID Application: <Name> (<TEAMID>)".
identity_team() { local re='\(([A-Z0-9]{10})\)$'; if [[ "$1" =~ $re ]]; then printf '%s' "${BASH_REMATCH[1]}"; fi; }

# How build-macos.sh notarizes, from the credentials that are set: api-key (App Store Connect
# API key, preferred), apple-id (Apple ID + app-specific password) or none.
notarize_mode() {
  if has_value APPLE_API_ISSUER && has_value APPLE_API_KEY && has_value APPLE_API_KEY_PATH; then
    echo api-key
  elif has_value APPLE_ID && has_value APPLE_PASSWORD && has_value APPLE_TEAM_ID; then
    echo apple-id
  else
    echo none
  fi
}

# ---------------------------------------------------------------------------
# The build's files
# ---------------------------------------------------------------------------
# For Rust host triple $1, sets ARCH_LABEL (in the zip's name), UPDATER_PLATFORM (latest.json's
# platform key) and TARBALL_NAME. Fails for a triple we don't build.
artifact_names() {
  case "$1" in
    aarch64-apple-darwin) ARCH_LABEL="arm64" ;;
    x86_64-apple-darwin)  ARCH_LABEL="x86_64" ;;
    *) return 1 ;;
  esac
  local arch="${1%%-*}"   # aarch64 / x86_64, as in the updater's platform names
  UPDATER_PLATFORM="darwin-$arch"
  TARBALL_NAME="hopp_$arch.app.tar.gz"
}
# The zip for version $1 and commit $2 (short sha, plus -dirty for a dirty tree).
zip_name() { printf 'Hopp-Dataico-%s-%s-%s.zip' "$1" "$2" "$ARCH_LABEL"; }
# Where the GitHub release of version $1 serves file $2.
release_download_url() { printf '%s/download/%s%s/%s' "$RELEASES_URL" "$TAG_PREFIX" "$1" "$2"; }

# A value from a JSON file (plutil reads JSON too), empty when it is missing. plutil prints its
# errors on stdout, so drop the output when it fails. Booleans come out as true / false.
json_get() { local v; v="$(plutil -extract "$2" raw -o - "$1" 2>/dev/null)" || return 0; printf '%s' "$v"; }

# Fails and prints why when build-info.json $1 doesn't hold the "field=value" pairs that follow.
build_info_mismatch() {
  local file="$1" pair got
  shift
  [[ -f "$file" ]] || { echo "no $(basename "$file") (that build didn't finish)"; return 1; }
  for pair in "$@"; do
    got="$(json_get "$file" "${pair%%=*}")"
    [[ "$got" == "${pair#*=}" ]] \
      || { echo "$(basename "$file") has ${pair%%=*} '$got', not '${pair#*=}'"; return 1; }
  done
}

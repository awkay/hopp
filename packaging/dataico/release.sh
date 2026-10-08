#!/usr/bin/env bash
#
# Release the Dataico app: release notes, version bump, signed + notarized build with
# the update files, tag, push and GitHub release. See DATAICO.md.
#
#   task -d packaging/dataico release VERSION=1.0.35
#   packaging/dataico/release.sh 1.0.35            # the same, without Task
#
#   DRY_RUN=1   stop after the build and print what would be pushed and published
#
# Running it again with the same VERSION continues where the last run stopped. The
# script keeps no state of its own; it looks at HEAD (the release commit), the build
# files in dist/dataico/ and their build-info.json, the tag on origin and the GitHub release.
#
# Needs what a notarized build with the updater needs (see build-macos.sh; shell or
# packaging/dataico/.env) and a logged-in `gh`.

set -euo pipefail
unset CDPATH   # a user CDPATH makes `cd` print paths and breaks $(cd ... && pwd)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_PREFIX="dataico-release"
# shellcheck source=SCRIPTDIR/lib.sh
source "$SCRIPT_DIR/lib.sh"
TAURI_CONF="tauri/src-tauri/tauri.conf.json"   # paths relative to the repo root
UPDATER_CONFIG="tauri/src-tauri/tauri.conf.dataico-updater.json"
CHANGELOG="CHANGELOG.md"
RELEASE_TRIPLE="aarch64-apple-darwin"   # releases are built on Apple Silicon only
# The last release without the update button: its users install the next one by hand.
LAST_MANUAL_VERSION="1.0.34"
# Housekeeping commits left out of the notes draft: these prefixes, or `cargo fmt` anywhere.
HOUSEKEEPING='^((chore|ci|docs|style|test)(\([^)]*\))?!?:|CI:)|cargo fmt'

VERSION="${1:-${VERSION:-}}"
[[ "$VERSION" =~ $VERSION_RE ]] \
  || die "Usage: task -d packaging/dataico release VERSION=<x.y.z>, no leading zeros (got '${VERSION}')"
DRY_RUN="${DRY_RUN:-}"
[[ "$DRY_RUN" == 0 ]] && DRY_RUN=""
TAG="$TAG_PREFIX$VERSION"
TITLE="Hopp for Dataico $VERSION"
RELEASE_SUBJECT="chore(release): Dataico $VERSION"

cd "$REPO_ROOT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# packaging/dataico/.env, without exporting anything: this script only checks the values, and
# build-macos.sh reads .env itself. Of the secrets it learns only whether they are set, and
# those the shell exported stop being exported here, so the editor, git hooks, gh and curl never
# get them; only the build does (step 2).
load_env check
# shellcheck disable=SC2163 # $name is the name of the variable to unexport
for name in "${SECRET_VARS[@]}"; do export -n "$name"; done

# gh comes from mise.toml when it isn't installed globally.
gh_run() { if command -v gh >/dev/null 2>&1; then command gh "$@"; else mise exec -- gh "$@"; fi; }
version_gt() { [[ "$1" != "$2" && "$(printf '%s\n' "$1" "$2" | sort -V | tail -1)" == "$1" ]]; }

# ---------------------------------------------------------------------------
# State: the release tags on origin and where this checkout stands
# ---------------------------------------------------------------------------
log "Reading $TAG_PREFIX* tags on origin and fetching main"
REMOTE_TAGS="$(git ls-remote --tags origin "$TAG_PREFIX*")" || die "Could not list the tags on origin."
git fetch --quiet origin main || die "git fetch origin main failed."
# Release versions only: no `^{}` (peeled) entries, no tags that aren't x.y.z.
RELEASED="$(awk '{print $2}' <<<"$REMOTE_TAGS" | sed -n "s|^refs/tags/$TAG_PREFIX||p" \
  | { grep -E "$VERSION_RE" || true; } | sort -V)"
# The commit a tag on origin points to (annotated tags list it as <tag>^{}).
remote_tag_commit() {
  awk -v ref="refs/tags/$1" '$2 == ref "^{}" {peeled = $1} $2 == ref {own = $1}
                             END {print (peeled != "" ? peeled : own)}' <<<"$REMOTE_TAGS"
}

LAST="$(tail -1 <<<"$RELEASED")"
PREV=""                    # the release before VERSION: the notes cover PREV..VERSION
FIRST_UPDATER_VERSION=""   # the first release that had the update button
for v in $RELEASED; do
  if version_gt "$VERSION" "$v"; then PREV="$v"; fi
  if [[ -z "$FIRST_UPDATER_VERSION" ]] && version_gt "$v" "$LAST_MANUAL_VERSION"; then FIRST_UPDATER_VERSION="$v"; fi
done
TAG_REMOTE="$(remote_tag_commit "$TAG")"
HEAD_SHA="$(git rev-parse HEAD)"
AT_RELEASE_COMMIT=0
[[ "$(git log -1 --format=%s)" == "$RELEASE_SUBJECT" ]] && AT_RELEASE_COMMIT=1
ORIGIN_MAIN="$(git rev-parse origin/main)"

# ---------------------------------------------------------------------------
# Refuse to start unless everything holds (a rerun accepts its own earlier progress)
# ---------------------------------------------------------------------------
problems=()
problem() { problems+=("$*"); }

[[ "$(uname -m)" == arm64 ]] || problem "Releases are built on Apple Silicon; this Mac is $(uname -m)."
branch="$(git symbolic-ref --short -q HEAD || echo "a detached HEAD")"
[[ "$branch" == main ]] || problem "Not on main (on $branch)."
[[ -z "$(git status --porcelain)" ]] || problem "The working tree isn't clean (see git status)."
git remote get-url origin | grep -Eq "github\.com[:/]$REPO_SLUG(\.git)?$" \
  || problem "origin is not github.com/$REPO_SLUG."
[[ -n "$PREV" ]] || problem "No $TAG_PREFIX* release lower than $VERSION on origin to write the notes from."

if [[ -n "$TAG_REMOTE" ]]; then
  # An earlier run pushed it; only the GitHub release can be left.
  [[ "$TAG_REMOTE" == "$HEAD_SHA" && "$AT_RELEASE_COMMIT" == 1 ]] \
    || problem "Tag $TAG already exists on origin (at ${TAG_REMOTE:0:7}). Pick a higher VERSION."
else
  [[ -z "$LAST" ]] || version_gt "$VERSION" "$LAST" \
    || problem "VERSION $VERSION is not higher than the last release, $LAST."
  local_tag="$(git rev-parse -q --verify "refs/tags/$TAG^{commit}" || true)"
  [[ -z "$local_tag" || ( "$local_tag" == "$HEAD_SHA" && "$AT_RELEASE_COMMIT" == 1 ) ]] \
    || problem "Tag $TAG exists locally but not on origin. Delete it: git tag -d $TAG"
  if [[ "$AT_RELEASE_COMMIT" == 1 ]]; then
    [[ "$ORIGIN_MAIN" == "$(git rev-parse HEAD^)" ]] \
      || problem "HEAD is the release commit, but its parent isn't origin/main. Drop it (git reset --hard HEAD^), pull and rerun."
  elif [[ "$ORIGIN_MAIN" != "$HEAD_SHA" ]]; then
    read -r behind ahead < <(git rev-list --left-right --count origin/main...HEAD)
    problem "main is not level with origin/main ($ahead ahead, $behind behind)."
    other="$(git log -1 --format=%s | sed -n 's/^chore(release): Dataico //p')"
    [[ -z "$other" ]] || problem "HEAD is an unpushed release commit for $other: rerun with VERSION=$other, or drop it (git reset --hard HEAD^)."
  fi
  [[ "$AT_RELEASE_COMMIT" == 1 || -f "$CHANGELOG" ]] || problem "$CHANGELOG not found at the repo root."
fi

# The team the release must be signed by (build-info.json's team_id).
EXPECTED_TEAM="$(identity_team "${APPLE_SIGNING_IDENTITY:-}")"
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  problem "APPLE_SIGNING_IDENTITY is not set (the Developer ID Application identity)."
elif [[ "$APPLE_SIGNING_IDENTITY" != Developer\ ID\ Application:* ]]; then
  problem "APPLE_SIGNING_IDENTITY is not a 'Developer ID Application' identity."
else
  [[ -n "$EXPECTED_TEAM" ]] \
    || problem "APPLE_SIGNING_IDENTITY doesn't end in its Team ID, as in \"Developer ID Application: <Name> (ABCDE12345)\"."
  has_value APPLE_CERTIFICATE || identity_in_keychain "$APPLE_SIGNING_IDENTITY" \
    || problem "Signing identity \"$APPLE_SIGNING_IDENTITY\" is not in the keychain."
fi
case "$(notarize_mode)" in
  api-key) [[ -f "$APPLE_API_KEY_PATH" ]] || problem "APPLE_API_KEY_PATH '$APPLE_API_KEY_PATH' does not exist." ;;
  none) problem "No notarization credentials: $NOTARIZE_CREDENTIALS." ;;
esac
has_value TAURI_SIGNING_PRIVATE_KEY || problem "TAURI_SIGNING_PRIVATE_KEY (the updater key) is not set."
has_value TAURI_SIGNING_PRIVATE_KEY_PASSWORD || problem "TAURI_SIGNING_PRIVATE_KEY_PASSWORD is not set."
[[ -n "$(json_get "$UPDATER_CONFIG" plugins.updater.pubkey)" ]] \
  || problem "plugins.updater.pubkey is empty in $UPDATER_CONFIG."
[[ "$(json_get "$UPDATER_CONFIG" plugins.updater.endpoints.0)" == "$FEED_URL" ]] \
  || problem "The feed in $UPDATER_CONFIG is not $FEED_URL."
[[ -z "${DATAICO_UPDATER_ENDPOINT:-}" ]] \
  || problem "DATAICO_UPDATER_ENDPOINT is set (a test feed); unset it in the shell or packaging/dataico/.env."
[[ -z "${VITE_API_BASE_URL:-}" || "$VITE_API_BASE_URL" == "$DEFAULT_API_BASE_URL" ]] \
  || problem "VITE_API_BASE_URL is set to $VITE_API_BASE_URL; releases use the default server."
gh_run auth status >/dev/null 2>&1 \
  || problem "gh is not logged in (gh auth login), or not installed (mise install, or brew install gh)."

if (( ${#problems[@]} )); then
  printf '[%s] Not releasing %s:\n' "$LOG_PREFIX" "$VERSION" >&2
  printf '  - %s\n' "${problems[@]}" >&2
  exit 1
fi

# The release on GitHub: none, draft or published.
release_state() {
  local out
  if out="$(gh_run release view "$TAG" --repo "$REPO_SLUG" --json isDraft --jq .isDraft 2>&1)"; then
    if [[ "$out" == true ]]; then echo draft; else echo published; fi
  elif grep -q 'release not found' <<<"$out"; then
    echo none
  else
    die "gh release view $TAG failed: $out"
  fi
}
STATE=none
[[ -z "$TAG_REMOTE" ]] || STATE="$(release_state)"
if [[ "$STATE" == published ]]; then
  log "$TITLE is already published: https://github.com/$REPO_SLUG/releases/tag/$TAG"
  exit 0
fi

# ---------------------------------------------------------------------------
# 1. Notes, version bump, CHANGELOG.md, release commit
# ---------------------------------------------------------------------------
DRAFT="$OUT_DIR/release-notes-$VERSION.md"   # dist/ is git-ignored

draft_notes() {
  local since commits
  since="$(remote_tag_commit "$TAG_PREFIX$PREV")"
  git fetch --quiet origin "refs/tags/$TAG_PREFIX$PREV"   # the tag's commit may not be local yet
  # main is rebased onto upstream, so old release commits aren't ancestors of HEAD and
  # `$since..HEAD` lists every replayed fork commit again. --cherry-pick drops the commits
  # whose patch the last release already had.
  commits="$(git log --no-merges --cherry-pick --right-only --format=%s "$since...HEAD")"
  cat <<EOF
# Release notes for $VERSION: what's new since $PREV. They go into CHANGELOG.md
# (the app's "What's new" tab), the GitHub release and the update feed.
#
# Rewrite the commits below for users: one bullet per change they can see, a bold
# lead, then where to find it. Drop what they can't see. Lines starting with '#'
# are removed. Leave the notes empty to stop.
#
# Example, from 1.0.34:
#
# - **Sharing buttons in the menu bar.** While you share your screen, the Hopp menu-bar icon grows a draw button and a stop-sharing button next to it. Turn it off in Settings › Call settings › "Show sharing buttons in menu bar".
# - **Favorite teammates.** Star a teammate (hover their row) to pin them in a Favorites section at the top of the list.
# - **Local drawing works every time.** Before, it failed on every second toggle.
# - **Esc closes the menu-bar popup.**
#
# Commits since $PREV, without housekeeping (chore, ci, docs, style, test, cargo fmt):

EOF
  if [[ -n "$commits" ]]; then grep -Ev "$HOUSEKEEPING" <<<"$commits" | sed 's/^/- /' || true; fi
}

# Drops '#' lines and leading / trailing blank lines.
strip_notes() {
  awk '!/^#/ { line[++n] = $0; if ($0 ~ /[^[:space:]]/) { if (!first) first = n; last = n } }
       END { for (i = first; first && i <= last; i++) print line[i] }' "$1"
}

# Prints the notes under "## <VERSION> (" in CHANGELOG.md.
changelog_notes() {
  awk -v head="## $VERSION (" '/^## / { if (found) exit; found = (index($0, head) == 1); next } found' "$CHANGELOG" \
    > "$WORK/section.md"
  strip_notes "$WORK/section.md"
}

if [[ "$AT_RELEASE_COMMIT" == 1 ]]; then
  log "Step 1/4: HEAD is already \"$RELEASE_SUBJECT\"; keeping its notes."
else
  log "Step 1/4: release notes"
  mkdir -p "$OUT_DIR"
  # Drafted in $WORK, so a failed or interrupted draft never becomes the $DRAFT that a rerun
  # reuses. Drafted on every run: it is what unedited notes are compared with.
  draft_notes > "$WORK/draft.md"
  strip_notes "$WORK/draft.md" > "$WORK/drafted-notes.md"
  if [[ -f "$DRAFT" ]]; then
    log "Reusing the draft from an earlier run: $DRAFT"
  else
    mv "$WORK/draft.md" "$DRAFT"
  fi
  editor="${EDITOR:-$(git var GIT_EDITOR)}"
  log "Opening the draft in $editor"
  # $editor may carry arguments (e.g. "code --wait"), so let sh split it, as git does.
  sh -c "$editor \"\$1\"" editor "$DRAFT" || die "The editor failed; the draft stays in $DRAFT."
  strip_notes "$DRAFT" > "$WORK/notes.md"
  if [[ ! -s "$WORK/notes.md" ]]; then
    rm -f "$DRAFT"
    die "The notes are empty; stopping. Nothing was changed."
  fi
  if cmp -s "$WORK/notes.md" "$WORK/drafted-notes.md"; then
    die "The notes are still the drafted commit list. Rewrite them for users (see the comments at the top), then rerun; the draft stays in $DRAFT. Nothing was changed.
  If the editor returned at once, make it wait: EDITOR=\"code --wait\", for example."
  fi

  sed -i '' -E "s/^(  \"version\": )\"[^\"]*\"/\\1\"$VERSION\"/" "$TAURI_CONF"
  [[ "$(json_get "$TAURI_CONF" version)" == "$VERSION" ]] || die "Could not set the version in $TAURI_CONF."
  # New section above the newest one, below the title and intro.
  awk -v head="## $VERSION ($(date +%Y-%m-%d))" -v notes="$WORK/notes.md" '
    function add() { print head; print ""; while ((getline l < notes) > 0) print l; done = 1 }
    !done && /^## / { add(); print "" }
    { print }
    END { if (!done) { print ""; add() } }
  ' "$CHANGELOG" > "$WORK/CHANGELOG.md"
  cat "$WORK/CHANGELOG.md" > "$CHANGELOG"

  git add "$TAURI_CONF" "$CHANGELOG"
  if ! git commit --quiet -m "$RELEASE_SUBJECT"; then
    git checkout HEAD -- "$TAURI_CONF" "$CHANGELOG"
    die "git commit failed; undid the version bump and $CHANGELOG. The notes stay in $DRAFT for the next run."
  fi
  rm -f "$DRAFT"
  HEAD_SHA="$(git rev-parse HEAD)"
  log "Committed $RELEASE_SUBJECT ($(git rev-parse --short HEAD))"
fi

changelog_notes > "$WORK/notes.md"
[[ -s "$WORK/notes.md" ]] || die "$CHANGELOG has no notes under \"## $VERSION (\"."

# ---------------------------------------------------------------------------
# 2. Build: signed, notarized, with the update files
# ---------------------------------------------------------------------------
SHORT_SHA="$(git rev-parse --short HEAD)"
artifact_names "$RELEASE_TRIPLE"
TARBALL="$OUT_DIR/$TARBALL_NAME"
FEED="$OUT_DIR/$FEED_NAME"
FILES=("$OUT_DIR/$(zip_name "$VERSION" "$SHORT_SHA")" "$TARBALL" "$TARBALL.sig" "$FEED")
DOWNLOAD_URL="$(release_download_url "$VERSION" "$TARBALL_NAME")"
NOTES_SHA256="$(shasum -a 256 "$WORK/notes.md" | awk '{print $1}')"

# Why the files in dist/dataico aren't this release's build; nothing when they are. A rerun
# (after DRY_RUN=1, or a failed push or upload) publishes them without building again, so
# build-info.json must show a finished build made exactly as this run would make it. Any local
# build, with another server or feed, or without the notes, deletes or changes it.
build_mismatch() {
  local f
  for f in "${FILES[@]}"; do [[ -f "$f" ]] || { echo "no $(basename "$f")"; return; }; done
  build_info_mismatch "$BUILD_INFO" version="$VERSION" git_sha="$HEAD_SHA" dirty=false \
    host_triple="$RELEASE_TRIPLE" api_base_url="$DEFAULT_API_BASE_URL" updater=true \
    updater_endpoint="$FEED_URL" release_notes_sha256="$NOTES_SHA256" team_id="$EXPECTED_TEAM" \
    notarized=true || return 0
  [[ "$(json_get "$FEED" version)" == "$VERSION" \
     && "$(json_get "$FEED" "platforms.$UPDATER_PLATFORM.url")" == "$DOWNLOAD_URL" ]] \
    || echo "$FEED_NAME is not $VERSION's feed, pointing to $DOWNLOAD_URL"
}

mismatch="$(build_mismatch)"
if [[ -z "$mismatch" ]]; then
  log "Step 2/4: dist/dataico already holds this release's build of $VERSION ($SHORT_SHA); not rebuilding."
else
  log "Step 2/4: building $VERSION ($SHORT_SHA); nothing to reuse in dist/dataico ($mismatch)"
  # The only child that gets the secrets the shell had exported (it reads .env's itself).
  (
    # shellcheck disable=SC2163 # $name is the name of the variable to export
    for name in "${SECRET_VARS[@]}"; do [[ -z "${!name+x}" ]] || export "$name"; done
    DATAICO_RELEASE_NOTES="$WORK/notes.md" exec "$SCRIPT_DIR/build-macos.sh"
  )
  mismatch="$(build_mismatch)"
  [[ -z "$mismatch" ]] || die "The build finished, but dist/dataico doesn't hold this release's build: $mismatch"
fi

release_body() {
  echo "Signed and notarized for Dataico SAS. Apple Silicon only."
  echo
  echo "**Install:** quit and delete the official Hopp app, unzip, drag hopp.app to Applications, grant Screen Recording, Accessibility, Camera and Microphone."
  if version_gt "$PREV" "$LAST_MANUAL_VERSION"; then
    echo "**Upgrading from $LAST_MANUAL_VERSION or earlier:** quit Hopp, then replace hopp.app in Applications with the new one."
    echo "**Already on $FIRST_UPDATER_VERSION or later:** click the update button in the sidebar."
  else
    echo "**Upgrading from $LAST_MANUAL_VERSION or earlier:** quit Hopp, then replace hopp.app in Applications with the new one, by hand one last time. From now on, Hopp shows an update button in the sidebar when a new version is out."
  fi
  echo
  echo "## What's new since $PREV"
  echo
  cat "$WORK/notes.md"
}
release_body > "$WORK/body.md"

if [[ -n "$DRY_RUN" ]]; then
  banner "DRY RUN: $VERSION is built; nothing was pushed or published."
  if [[ -z "$TAG_REMOTE" ]]; then
    log "Would tag $TAG at $SHORT_SHA and run: git push --atomic origin main $TAG"
    log "Commits that would go to origin/main:"
    git log --format='    %h %s' origin/main..HEAD
  fi
  log "Would create the draft release \"$TITLE\" with:"
  printf '    %s\n' "${FILES[@]#"$REPO_ROOT"/}"
  log "then publish it as the latest release, so $FEED_URL serves $VERSION. Its body:"
  sed 's/^/    | /' "$WORK/body.md"
  if [[ -z "$TAG_REMOTE" ]]; then
    log "The release commit stays local. Rerun without DRY_RUN to publish, or drop it: git reset --hard HEAD^"
  fi
  exit 0
fi

# ---------------------------------------------------------------------------
# 3. Tag; push main and the tag together
# ---------------------------------------------------------------------------
if [[ -n "$TAG_REMOTE" ]]; then
  log "Step 3/4: $TAG is already on origin."
else
  log "Step 3/4: tagging $TAG and pushing main with it"
  git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || git tag -a "$TAG" -m "$TITLE ($SHORT_SHA)"
  git push --atomic origin main "$TAG"
fi

# ---------------------------------------------------------------------------
# 4. GitHub release: a draft with every file, then published as latest
# ---------------------------------------------------------------------------
# Until it is published, releases/latest/download/latest.json still serves the previous
# release, so the feed never points to files that aren't uploaded yet.
log "Step 4/4: GitHub release"
if [[ "$STATE" == draft ]]; then
  log "Uploading the files to the existing draft"
  gh_run release upload "$TAG" --repo "$REPO_SLUG" --clobber "${FILES[@]}"
else
  gh_run release create "$TAG" --repo "$REPO_SLUG" --draft --verify-tag \
    --title "$TITLE" --notes-file "$WORK/body.md" "${FILES[@]}"
fi

assets="$(gh_run release view "$TAG" --repo "$REPO_SLUG" --json assets --jq '.assets[] | "\(.name) \(.size)"')"
for f in "${FILES[@]}"; do
  grep -qxF "$(basename "$f") $(stat -f %z "$f")" <<<"$assets" \
    || die "The draft release lacks $(basename "$f") (or its size differs). Rerun to upload again."
done

gh_run release edit "$TAG" --repo "$REPO_SLUG" --title "$TITLE" --notes-file "$WORK/body.md" \
  --draft=false --latest >/dev/null

served="$(curl -fsSL "$FEED_URL" 2>/dev/null | plutil -extract version raw -o - - 2>/dev/null || true)"
if [[ "$served" == "$VERSION" ]]; then
  feed_line="The feed serves $VERSION."
else
  feed_line="The feed doesn't serve $VERSION yet (GitHub can take a minute): $FEED_URL"
fi
banner "RELEASED: $TITLE" \
       "https://github.com/$REPO_SLUG/releases/tag/$TAG" \
       "$feed_line"

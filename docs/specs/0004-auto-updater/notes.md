# 0004 notes: quirks, gotchas and bugs

Discoveries while building in-app updates and the release task. The local rehearsal ran on
2026-10-08 (below); the signed end-to-end test and the first real release haven't run yet. Add what
they find here.

## Bugs fixed along the way

- **B4 was half broken before this spec.** The `incoming_call` listener in `app.tsx` is registered
  once and read `updateInProgress` from the first render, so incoming calls were never rejected
  during an update (invites were, via `getState()`). It now reads the store.
- **Sign-out hid the update button.** `reset()` wiped `needsUpdate` until the next 15-minute poll.
  The update version and the "What's new" tile now survive sign-out.
- **The old spinner never stopped.** A failed download left `updateInProgress` set, so the button
  spun and calls were rejected until restart. B5 resets it.
- `build-macos.sh` had 7 existing shellcheck findings (SC2015, SC2012 ×4, SC2155, SC2086), fixed.
- **The app didn't build at all.** `tauri build` refused with "Found version mismatched Tauri
  packages ... tauri-plugin-log (v2.8.0) : @tauri-apps/plugin-log (v2.10.0)": the npm package was
  added as `~2`, which resolved to 2.10.0, while `Cargo.lock` has the crate at 2.8.0. Pinned `~2.8`
  like the other plugins (`~2.10`, `~2.7`, `~2.5`). `tsc` and clippy never see this; only a real
  `tauri build` does, so `build-macos.sh` and the release task would have failed. CI's
  `pre-commit` job now runs `scripts/check-tauri-versions.mjs`, which applies tauri-cli's rule
  (`check_mismatched_packages`: `tauri` ↔ `@tauri-apps/api` and each `tauri-plugin-X` ↔
  `@tauri-apps/plugin-X` must match in major.minor, npm side as the `tauri` workspace resolves it).
  It lives in the root `scripts/` because `.gitignore`'s `*/scripts` ignores `tauri/scripts/`.

- **Found by the pre-PR review and fixed:**
  - A release rerun published whatever matching build sat in `dist/dataico/`, even one made for a
    dev server or test feed. Now only a matching `build-info.json` lets it reuse a build.
  - `release.sh` exported the signing secrets to every child (editor, git hooks, gh).
  - `build-macos.sh` left temp files and a partial app copy behind when it died.
  - Unedited notes (the raw commit list) were accepted; a failed draft left an empty file that the
    rerun reused; `VERSION` accepted leading zeros, which Tauri rejects only after the release
    commit.
  - Updates from `check()` were never closed; neither request had a timeout, so a stalled download
    left calls rejected until restart; a release pulled between poll and click left the tile stale
    with a misleading toast.

## Updater and Tauri

- tauri-cli 2.10.1 merges repeated `--config` flags in order (CLI 2.4.0, #12970). Config strings
  (pubkey, endpoints) are embedded verbatim in the binary, so `grep` on the binary checks them.
- With `endpoints: []`, `check()` fails at once with "Updater does not have any endpoints set."
  and makes no request. Polling treats it as "updater off" and stops.
- `tauri signer sign` accepts only the key's contents: a path in `TAURI_SIGNING_PRIVATE_KEY` fails
  with "failed to decode base64 secret key", though `tauri build` accepts a path. Without a
  password and without a TTY it fails with "Device not configured (os error 6)".
- The macOS updater (plugin 2.10.1) drops the first path component of every tar entry and installs
  the rest as the app. The tarball must hold only `hopp.app`; an AppleDouble `._hopp.app` entry
  would break the install (hence `COPYFILE_DISABLE=1`). macOS bsdtar 3.5.3 stores xattrs
  (quarantine, provenance) as pax headers by default; `--no-xattrs` makes the tarball match what
  the updater installs. The notarized 1.0.34 app round-tripped through it passes `codesign --verify
  --deep --strict`, `stapler validate` and `spctl`.
- In the menu-bar style, `setup_tray_icon` places the popup under the tray at launch only while it
  is hidden, after polling up to ~10 s for the tray's position, and sets `location_set` *before*
  placing. Showing the window from JS earlier leaves it where it was created. Hence
  `AppData::main_window_placed` and `show_main_window_when_placed`.
- B10's wait is `wait_for_flag` in `lib.rs`, tested with `#[tokio::test(start_paused = true)]`
  (tokio `test-util` dev-dependency, which leaves `Cargo.lock` unchanged). It must use tokio's clock:
  paused time doesn't advance `std::time::Instant`, so a `std` deadline would hang the test.
- `tauri-plugin-log` files frontend messages under `webview:<location>` targets, and `main.rs`
  sets the global level to Warn (only `hopp*` gets `log_level`): only JS `warn()`/`error()` reach
  `hopp.log`. `hopp.log` is truncated at every launch.
- Clippy and tests in `tauri/src-tauri` need `SENTRY_DSN_RUST` set at compile time (CI sets `""`),
  besides `tauri/dist/` and an empty `core/target/debug/hopp_core-aarch64-apple-darwin`.

- The updater plugin (2.10.1) never frees the update object `check()` returns, not even after
  `downloadAndInstall`; only an explicit `close()` does (the main window may call it through
  `core:default`). A 15-minute poll that never closes leaks one per poll while an update goes
  unclicked.
- The plugin's `timeout` becomes reqwest's total timeout, which covers the whole body. The update
  object `check()` returns has no timeout of its own, so the download needs its own value. The
  1.0.34 zip is 41.5 MB, so the 15-minute cap allows anything above about 46 KB/s.
- Download progress events and the result of `downloadAndInstall` reach the page by different
  routes, so their order isn't guaranteed. `update.ts` drops progress events that arrive after a
  failure, so the tile can't get stuck on "Updating".

## Frontend

- A native `disabled` button never opens a Radix tooltip (no pointer events), so B3 uses
  `aria-disabled` and a guard in `onClick`.
- `tauri/tsconfig.json` has `noUncheckedIndexedAccess`: `split(...)[0]` is `string | undefined`.
- `tauri/` resolves Vite 5.4.21 from its own `node_modules`; the root has 6.4.2. The dev server's
  default `fs.allow` (`searchForWorkspaceRoot`) is the repo root, because the root `package.json`
  has `workspaces`, so `../../../CHANGELOG.md?raw` works in `tauri dev` and in builds without
  config. `src/vite-env.d.ts` already types `?raw`.
- `App.css` styles bare elements globally (`ul` margins, `li` `mt-2`, large `h3`/`h4`, `p`
  `leading-7`), so markdown output needs explicit classes on every element. Tailwind v4
  `space-y-*` skips the last child, so a base `li` margin survives on the last bullet.
- `react-markdown` with `skipHtml` drops raw HTML tags but keeps the text between them: an HTML
  comment disappears and `<b>x</b>` renders as "x".

## Release task and tooling

- On Ctrl-C, bash 5.3 runs the EXIT trap with `$?` = 0 unless an INT trap exists (bash 3.2
  reports 130). A cleanup that checks the exit status needs `trap 'exit 130' INT` (and TERM).
- A `die` inside `( … )` or `$( … )` doesn't run the parent's EXIT trap, so subshell functions
  such as `updater_sign` can't trigger the cleanup early.
- Non-interactive bash runs `&` jobs with SIGINT ignored, so `kill -INT` doesn't simulate Ctrl-C;
  start the job in its own process group (`perl -e '$SIG{INT}="DEFAULT"; setpgrp(0,0); exec @ARGV'`)
  and signal the group.
- `export -n` works on bash 3.2 (un-exports, keeps the value). `( export NAME; exec cmd )` hands a
  secret to one child without putting it on a command line.
- The scripts use `#!/usr/bin/env bash`: bash 5 with Homebrew, `/bin/bash` 3.2 on a plain Mac, so
  they stay 3.2-compatible.
- The `.env` loader is line-based: a quoted value spanning lines fails with an `eval` syntax error.
  In `release.sh`'s check-only mode a line referring to a secret (`X=$TAURI_SIGNING_PRIVATE_KEY`)
  expands to empty, since secret values never enter `release.sh`. A `GH_TOKEN` kept only in `.env`
  no longer reaches `gh`.
- `plutil -extract … raw` prints JSON booleans as `true`/`false` and accepts array indexes in key
  paths (`plugins.updater.endpoints.0`).
- `build-info.json` carries a notes hash because a plain `task build` at the release commit (no
  notes file) otherwise matched every other field and would have published empty notes.
- A5's guard compares against a fresh draft, so if `origin/main` moves between two runs that both
  leave the notes unedited, the stale draft passes.
- `yarn prettier` works only from the repo root (prettier is a root dependency); from `tauri/` it
  fails with "Couldn't find a script named prettier".

- `main` is rebased onto upstream, so old release tags aren't ancestors of `main`.
  `git log --cherry-pick --right-only <last tag>...HEAD` gives 22 commits from 1.0.33 to 1.0.34
  (17 after the housekeeping filter), instead of 54.
- `git ls-remote --tags` lists annotated tags twice; `<tag>^{}` is the peeled commit.
- `plutil -extract` prints its errors on stdout, so a bare `$(plutil …)` returns the error text.
- Task CLI variables (`task release VERSION=x DRY_RUN=1`) aren't exported to the environment, and
  `shellQuote` on an unset variable is an error, hence `default ""`.
- The release-task checks run `git fetch origin main`, which updates `origin/main` for every
  worktree of the clone.
- Yarn 4.9.2 breaks on Node 24. Without a global yarn, run it under mise's Node 20:
  `mise exec -- node .yarn/releases/yarn-4.9.2.cjs <args>` (in zsh, wrap it in a shell function: a
  `$Y` variable holding the command isn't word-split). A new worktree needs its own
  `yarn install`.
- Tools that aren't installed (`shellcheck`, `gh` on this machine) run without changing the global
  setup via `mise exec <tool>@latest -- <tool> <args>`.
- The 1.0.32 GitHub release body has only the install header (it was the fork's first release), so
  `CHANGELOG.md` says "First Hopp for Dataico release." Seeded dates are the UTC day of
  `published_at` (1.0.34 was 10-07 in Colombia); the task's `date +%F` uses local time.

## Local rehearsal (2026-10-08)

Unsigned builds 1.0.35 and 1.0.36 from this branch (version and `CHANGELOG.md` bumped in a
throwaway worktree, never committed), signed with a throwaway minisign key, and a local feed: a
small Python server on `http://127.0.0.1:8765` that throttled the download and logged every request.
The test overlay set `dangerousInsecureTransportProtocol: true`; the plugin rejects non-https
endpoints in release builds without it.

Seen on screen and in the feed and `hopp.log`:
- B1: the tile appeared at launch; no download until the click (the feed showed only checks).
- B2/B10/B11/B12 in the Menu bar style: the ring filled over the throttled download, the app
  relaunched as 1.0.36 with the "Hopp updated" toast once, and the "New" tile and the "What's new"
  tab worked. The update was repeated in the Floating and Regular styles.
- B5: cutting the feed mid-download logged `Update failed: error decoding response body`, showed
  the toast, returned the tile to "Update" and installed nothing; the retry then updated.
- B6: a `latest.json` carrying the right key's signature of a different file downloaded fully,
  then failed with `Update failed: The signature verification failed`; nothing was installed.
  The toast still says "Check your connection", which fits this case badly; it should only
  happen with a broken release, so it stays.
- B9: no "Automatic updates" row. AeroSpace (Floating, Regular): after the relaunch the user stayed
  on the workspace they updated from.

Not covered (still for the signed test): B3 (in-call tile), B11 after a manual install, the Menu
bar style under AeroSpace (`aerospace list-windows --all` lists no Hopp window), B7 (grants carry
over) and Gatekeeper on the updated app.

Rehearsal quirks, all from the unsigned builds:
- The plugin's macOS install never checks code signatures: it verifies the minisign signature,
  extracts the tarball and swaps the bundle (an admin-privileges AppleScript only when the move is
  denied). That's why unsigned builds can rehearse the client flow at all.
- Every unsigned build loses the Screen Recording and Accessibility grants, so the relaunched app
  opens the Permissions window first.
- `compareVersions` ignores prerelease suffixes, so test versions must be plain `x.y.z`
  (`1.0.35-test.1` → `-test.2` never counts as an upgrade for B11).
- With the Regular style selected, a launch logged `center_window_on_tray: Tray center ... is
  outside all monitors, skipping` (tray rect at y = -24) 10 s after launch, i.e. after the
  placement task's ~10 s of polling. `center_window_on_tray` only runs in the Menu bar style
  (`setup_tray_icon` returns before spawning that task in the Dock styles), so that launch must
  have been running as Menu bar (a style change applies on the next launch). When centering is
  skipped like this,
  `main_window_placed` is still set, so B10 shows the window where it was created, not under the
  tray icon.

## Remaining limitations

- **Real `gh` hasn't created a release yet.** release.sh's create/upload/publish calls were tested
  against a stub. Checked with real gh 2.102.0: a missing release prints "release not found" and
  exits 1, which is what release.sh matches. `mise install` of `gh` fails with mise 2025.9.15
  ("GitHub attestations verification failed for aqua:cli/cli@2.102.0"); use `brew install gh` or a
  newer mise.
- **Seen on screen only in the unsigned rehearsal** (above). B3, B7, B11 after a manual install and
  the Menu bar style under AeroSpace are still unverified.
- If the tray has no rect at launch (`setup_tray_icon: Initial tray rect not available`) or its
  rect is off-screen (seen in the rehearsal), B10 shows the window where it was created instead of
  under the tray icon. With no rect, the popup also doesn't follow tray moves for that session (as
  upstream); a tray click still places it.
- On the first run of the first build with this feature, the version the user came from is
  unknown; the tab marks only the running version as new.
- Whether a non-admin macOS user can install an update in `/Applications` is untested
  (Verification step 11).
- Clicking "Update" relaunches the app as soon as the install finishes. A call, room join or invite
  started during the download is dropped by that relaunch. Accepted for now.
- Test builds (`DATAICO_UPDATER_ENDPOINT`) are signed with the production updater key. A test release
  created without the prerelease flag becomes GitHub's "latest", so production apps would install it
  and keep checking the test feed from then on. Always create test releases with `--prerelease`. A
  separate test keypair would close this gap; deferred.
- The minisign signature covers only the tarball bytes. `latest.json`'s `version` and `url` are
  unsigned, and every release ships the same tarball name. Whoever can edit our GitHub releases can
  therefore offer any build we ever signed as an update, including older and test builds. After the
  relaunch, `handleAppStart` logs a warning when the running version differs from the one offered.
  A real fix (refusing unless the verified bundle's version matches) would need a custom Rust
  install command; deferred.

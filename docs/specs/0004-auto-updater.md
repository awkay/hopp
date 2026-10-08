# 0004: In-app updates and a release task

## Goal

Today each release means asking everyone to quit Hopp, download the zip from the GitHub release and
replace `hopp.app` by hand (14 downloads of 1.0.34 so far), so people stay on old builds. With this
change, the app notices a new release and shows an update button. One click downloads, installs
and relaunches it. Nothing installs without the user asking. After an update, the app confirms it
and offers the release notes, without opening them unasked.

Releases are also done by hand today. One person bumps the version, builds on their laptop, writes
the notes and uploads the zip (checked 2026-10-08: the fork's Actions has never run
`release.yml`, and every release file was uploaded from gabo963's account). This adds one task that
does the whole release and keeps a `CHANGELOG.md`, which the app also shows as "What's new".

The client mostly exists and is only switched off in our build (an empty endpoint list; that
section of `DATAICO.md`, "How the updater is disabled", is now "How in-app updates work"): the
Tauri updater plugin, a check at startup and every 15 minutes (`tauri/src/lib/auto-update.ts`),
and the sidebar update button. We drop upstream's automatic part,
which downloads in the background and installs and relaunches when the window is unfocused.

## Behavior

**App**

- **B1.** When a release build of the Dataico app starts, and every 15 minutes after, the app shall
  fetch our update feed. If the feed's version is higher than the running version, the sidebar
  shall show the update tile: an icon over the label "Update", tinted blue, in the bottom group
  above the avatar, with the tooltip "Update to <version>. Hopp restarts." It shows whether or not
  the user is logged in. The app shall download nothing until the user clicks it.
- **B2.** When the user clicks the update tile, the app shall download the update while the tile
  shows a ring filling with the download's progress, the label "Updating" and the tooltip
  "Downloading <version>… <n>%". It shall then install it while the tile shows a spinner, the label
  "Restarting" and the tooltip "Installing, Hopp will restart", and relaunch. When the download
  size is unknown, the ring is a spinner.
- **B3.** While there is call activity (in a call, calling, ringing, inviting or invited), the update
  tile shall be grayed out and ignore clicks, and its tooltip shall read "Update after the call".
- **B4.** While an update is downloading or installing, the app shall reject incoming calls and
  invites. *(Existing.)*
- **B5.** When the download or install fails, or doesn't finish within 15 minutes (a check gives up
  after 30 s), the app shall return the tile to its B1 state, show an error toast ("Couldn't update
  Hopp. Check your connection and try again."), write the error to `hopp.log`, and accept calls
  again. *(Today the spinner never stops and calls stay rejected until restart.)* If the feed no
  longer offers an update when the user clicks (the release was pulled or rolled back), the tile
  goes away without a toast and the app logs a warning.
- **B6.** The app shall install an update only if its signature matches the updater public key built
  into the app. A failed background check writes to `hopp.log` and shows nothing.
- **B7.** After an update, the user shall stay logged in, keep their settings, and keep their Screen
  Recording, Accessibility, Camera and Microphone grants. (Grants carry over because every release is
  signed with the same Developer ID team and bundle ID.)
- **B8.** Ad-hoc signed builds, signed builds that aren't notarized, and dev builds shall never
  check for updates. A build from a checkout (`task -d packaging/dataico install`) never offers a
  release unless it is notarized with the updater key, which makes it a release build that checks the
  feed like any other.
- **B9.** Settings shall no longer show the "Automatic updates" toggle.
- **B10.** When the app starts as the version the user just updated to from the update tile, it shall
  show the main window once, in any window style, with the toast "Hopp updated to <version>".
- **B11.** When the app starts with a higher version than the last one it ran (by the update tile or
  by a manual install), the update tile's place shall show a "What's new" tile: an icon over the
  label "New", neutral gray, tooltip "What's new in <version>". It shall stay until the user opens
  it, or for 7 days. The app shall never open the notes by itself. On the first run of a build with
  this feature there is no last version yet: the tile shows only if the user is already logged in
  (an existing user, not a new install). A pending update (B1) takes the slot over the tile.
- **B12.** The avatar menu shall always have a "What's new" item. It and the B11 tile open a "What's
  new" tab in the main window with the notes from the `CHANGELOG.md` built into the app, newest
  first. Versions newer than the one the user came from (B11) are marked new; older ones are muted.
  Opening the tab clears the B11 tile.

**Release task** (`task -d packaging/dataico release VERSION=1.0.35`)

- **B13.** The task shall refuse to start, and say why, unless all of these hold: on `main`, clean,
  and level with `origin/main`; `VERSION` is higher than the last `dataico-v*` release; tag
  `dataico-v<VERSION>` doesn't exist; Developer ID identity, notarization credentials and updater
  key are set; `gh` is logged in.
- **B14.** The task shall write a draft of the notes. It lists the commits since the last release,
  comparing commits by content so it survives rebases. Housekeeping commits are dropped. The draft
  opens in `$EDITOR` for the releaser to rewrite into user-facing notes. The task shall stop if the
  notes come back empty or unedited (identical to the drafted commit list).
- **B15.** The task shall bump `version` in `tauri.conf.json`, add the notes at the top of
  `CHANGELOG.md` under `## <VERSION> (<date>)`, and commit both as `chore(release): Dataico
  <VERSION>`.
- **B16.** The task shall build the signed, notarized app with its update files (zip,
  `hopp_aarch64.app.tar.gz` with its signature, `latest.json`). The app bundles the `CHANGELOG.md`
  from the B15 commit, so it carries its own notes (B12).
- **B17.** Only after the build succeeds shall the task tag the commit, push `main` and the tag
  together (`git push --atomic`), and create the GitHub release as a draft. The draft gets the
  standard install header, the notes under "What's new since <last version>", and all files.
  Only then shall it publish the release as latest. The feed never points to files that haven't
  been uploaded yet.
- **B18.** When a step fails, nothing that comes after it shall happen. Running the task again with
  the same `VERSION` shall continue from the failed step, without repeating the notes or the commit.
  With `DRY_RUN=1`, the task shall stop after the build and print what it would push and publish.

Older clients: 1.0.34 and earlier have the updater off, so everyone installs the first
updater-enabled release by hand once (its release notes say so). The official upstream app
(`com.hopp.app`) never reads our feed. Neither do we read theirs. No IPC, protocol or backend change,
so calls between versions are unaffected.

Out of scope: background download or install; release notes inside the update tile or before
updating; releasing from CI; Windows/Linux; beta channels and staged rollouts; forcing a minimum
version; delta updates; a badge on the menu-bar icon (add it only if people still don't update).

## Design

**Feed: GitHub Releases on `awkay/hopp`.** The repo is public (checked 2026-10-08: the API reports
`private: false`, and release files download without logging in). The `dataico-v*` releases already
live there. Endpoint: `https://github.com/awkay/hopp/releases/latest/download/latest.json`. GitHub
redirects `latest/download/<file>` to the newest release that is neither a draft nor a prerelease.
Fetching it is a normal download, not a call to GitHub's rate-limited API. Drafts and prereleases
can be staged without shipping. If the repo ever goes private, the feed returns 404, and the files
would have to move to `hopp.apps.dataico.world`.

**Versioning.** Our version line is our own. Upstream is at 1.0.32 (`gethopp/hopp` `main`) and we
are at 1.0.34. The updater compares semver, so every release must be higher than the last. B13
enforces this with `sort -V` against the last `dataico-v*` tag. When an upstream rebase conflicts
on `tauri.conf.json` `version`, keep ours.

**Signing key.** A Tauri minisign keypair (`yarn tauri signer generate`), password-protected. The
public key goes in the config. The private key and its password are kept in the team's password
manager and passed as `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
(shell or `packaging/dataico/.env`). Whoever releases needs it, next to the Developer ID
certificate. `release.sh` never exports the secrets: it only checks that they are set, and only
`build-macos.sh` receives them. If the private key is lost, no installed app can be updated again: everyone would need
to reinstall by hand a build with a new public key.

**Config (B8).** `tauri.conf.dataico.json` keeps `endpoints: []`. A new overlay
`tauri.conf.dataico-updater.json` holds `pubkey` and our endpoint. `build-macos.sh` adds it only when
the build is notarized and the signing key is set. A notarized build without the key prints a loud
warning and builds with the updater off. So does an empty `pubkey` (as the overlay had until the
keypair was generated). `tauri build` merges repeated `--config` flags in order (tauri-cli 2.4.0+).
`DATAICO_UPDATER_ENDPOINT` overrides the endpoint for testing, and `latest.json` then points to the
tarball next to that endpoint, so a test prerelease holds both. Config strings are embedded verbatim
in the binary: the script checks the feed URL and pubkey are in it (and that an updater-off build
has no feed URL). Right after `yarn install` it also signs a probe file and verifies it against the
pubkey, so a wrong key or password fails before the long build.

**Update files (B16).** `createUpdaterArtifacts` stays `false`. `build-macos.sh` creates the files
itself after its last sign/notarize/staple step, because it can re-sign and re-notarize after Tauri
finishes. A tarball made by Tauri could then contain an app without the stapled ticket or with the
old signature. Steps:
`COPYFILE_DISABLE=1 tar --no-xattrs -czf hopp_aarch64.app.tar.gz -C <bundle dir> hopp.app`,
then `yarn tauri signer sign` for the `.sig`, then `latest.json`. The macOS updater drops the first
path component of every tar entry and installs the rest as the app, so the tarball holds only
`hopp.app` (an AppleDouble `._hopp.app` would break it), and `--no-xattrs` keeps quarantine and
provenance xattrs out. The script extracts the tarball and runs `codesign --verify` and
`stapler validate` on the result. `latest.json`:

```json
{ "version": "1.0.35", "notes": "<the release notes>", "pub_date": "<RFC 3339>",
  "platforms": { "darwin-aarch64": {
    "signature": "<contents of .sig>",
    "url": "https://github.com/awkay/hopp/releases/download/dataico-v1.0.35/hopp_aarch64.app.tar.gz" } } }
```

`build-macos.sh` takes the notes file as `DATAICO_RELEASE_NOTES` and stays usable on its own. The
release task calls it. `tauri signer sign` accepts only the key's contents (a path fails, though
`tauri build` accepts one), so the script reads a key file itself and passes secrets through the
environment, not argv.

**Release task.** A `release` task in `packaging/dataico/Taskfile.yml` runs the new
`packaging/dataico/release.sh`, which holds the steps of B13–B18. It runs on the releaser's laptop,
like today's builds. We rejected a CI workflow: it needs the Developer ID `.p12`, notarization key
and updater key as repo secrets, and releasing from a laptop works.
- *Notes range (B14).* `main` is rebased onto upstream, so old release tags drop out of its history.
  `dataico-v1.0.32` and `dataico-v1.0.33` are no longer ancestors of `main`, and a plain
  `<tag>..HEAD` range lists every replayed fork commit again. The task uses
  `git log --no-merges --cherry-pick --right-only <last tag>...HEAD`, which skips commits whose
  patch is already in the last release. From 1.0.33 to 1.0.34 that gives 22 commits instead of 54,
  exactly the ones behind the 1.0.34 notes. Commits that changed while resolving a rebase conflict
  can still show up again; the edit step catches them. The last tag comes from
  `git ls-remote --tags origin 'dataico-v*'`.
- *Draft shape (B14).* Commits starting `chore`, `ci`, `CI:`, `docs`, `style`, `test` or containing
  `cargo fmt` are dropped. Everything else is listed with its subject. The draft carries the
  1.0.34 notes as a style example in a comment, which the task strips: one bullet per user-visible
  change, bold lead, where to find it. We rejected git-cliff with upstream's `cliff.toml`: it has
  no patch comparison, links to `gethopp` PRs, and drops non-conventional subjects (`macOS: …`,
  `core: …`), which make up most fork commits.
- *Release body (B17).* A fixed header ("Signed and notarized for Dataico SAS. Apple Silicon only.",
  the install steps from today's releases, and "Already on 1.0.35 or later: click the update button
  in the sidebar"), then `## What's new since <last>` and the notes. The first updater-enabled
  release says "Upgrading from 1.0.34 or earlier: replace hopp.app by hand one last time" instead.
  Title: `Hopp for Dataico <VERSION>`, as today.
- *Resume (B18).* The script checks state instead of keeping its own: whether HEAD is the
  `chore(release)` commit for `VERSION`, whether `dist/dataico/build-info.json` shows a finished
  build of HEAD made for this release (version, commit, clean tree, default server, our feed,
  updater on, Developer ID team, notarized, these notes; `build-macos.sh` deletes it when a build
  starts and writes it last), whether the tag is on `origin`, and whether the release is a draft or
  published.
  It continues from the first step that hasn't happened yet.
- *Extra refusals* beyond B13: `DATAICO_UPDATER_ENDPOINT` set, a non-default `VITE_API_BASE_URL`,
  a Mac that isn't arm64, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` missing, an empty `pubkey`, an
  updater-config endpoint that isn't the production feed, an `APPLE_SIGNING_IDENTITY` without its
  `(TEAMID)`, a `VERSION` with leading zeros (Tauri's semver parser rejects them only after the
  release commit), and `origin` not `awkay/hopp`. All problems are listed at once.
- *Notes file.* The draft stays in `dist/dataico/release-notes-<VERSION>.md` until the commit and is
  reused by a rerun. It is drafted in a temp dir and moved into place only once complete, and
  drafted again on every run to tell unedited notes apart. Lines starting with `#` are comments and stripped, so notes have no headings
  (which also keeps `CHANGELOG.md`'s `## ` sections intact). Editor: `$EDITOR`, else git's editor.
- *Release body.* Every body says how to upgrade from 1.0.34 or earlier by hand. Every body after
  the first updater-enabled release also says "Already on <first updater version> or later: click
  the update button in the sidebar"; the first one says "by hand one last time" instead.
- `gh` goes into `mise.toml`.

**`CHANGELOG.md`** at the repo root, newest first, one `## <version> (<YYYY-MM-DD>)` section per
release holding only the user-facing notes (no install header). Seed it with 1.0.32–1.0.34 from the
existing GitHub release notes. Upstream has no such file, so rebases don't conflict on it.

**Update tile (B1–B5).** It replaces the gray, icon-only `DownloadNewVersionButton` in
`components/sidebar/Sidebar.tsx`, in the same slot of the bottom (app and account) group. The
sidebar's zones: top is navigation, middle is the call, bottom is app-level, so an update belongs at
the bottom, away from the green call button. A ~40 px tile, icon over a 10 px label, because a
rarely seen icon alone is never learned. Blue tint (`bg-blue-50`, `border-blue-200`,
`text-blue-700`): green means "in a call" and orange/red are the trial countdown's warnings. Same
hover background as `SidebarButton`; no `hover:scale`, which blurs the label. B3 uses
`aria-disabled` and a guard in `onClick`, not `disabled`: a disabled button emits no pointer events,
so the Radix tooltip would never open. B2's ring comes from the `Started` (`contentLength`) and
`Progress` (`chunkLength`) events that `downloadAndInstall` already reports, and `Finished` switches
to the install state. The store keeps the update's version (`check()` returns `update.version`), not
only a boolean. No confirmation dialog: B3 already rules out a call, and a restart costs seconds.
One entry point; no menu-bar badge (see Out of scope).

**After the update (B10–B12).** All state is in the main window's `localStorage`, which persists
across launches; no Rust settings change.
- B10: on click, before downloading, the app stores the target version as a pending-update marker
  (written at click time, so WebKit has the download's seconds to persist it before the relaunch).
  B5 clears it. At startup the app reads and clears it; if it differs from the running version, it
  logs a warning with both (an update cut short by quitting or a crash, or a feed serving an older
  signed build under a higher number). If it equals the running version, it calls
  the `show_main_window_when_placed` command, then shows the toast. In the menu-bar style,
  `setup_tray_icon` places the popup under the tray at startup only while it is hidden, after
  polling up to ~10 s for the tray's position, and its `location_set` flag is set before placing.
  So it now sets `AppData::main_window_placed` after that placement attempt, and the command waits
  for that flag (15 s cap) before `show_main_window`. The floating and regular styles show at once.
  If `center_window_on_tray` skips an off-screen tray rect, the flag is still set and the window
  shows where it was created. If the tray has no rect at all at launch, the task logs a warning and
  still sets the flag, so the window also shows where it was created. The 15 s cap (the tray task's
  ~10 s `TRAY_POSITION_MAX_WAIT` plus 5 s) is only a safety net: on timeout the command logs that the
  window was never placed and doesn't show it.
- B11: the app records the last version it ran. At startup, if the running version is higher, it
  stores the version it came from and an expiry 7 days out, which drive the tile. Opening the tab
  or the expiry clears them (also on a timer, since a menu-bar app runs for weeks). Notes still
  unread from an earlier upgrade stay marked new after a further one. On the first run without a
  recorded version, the version the user came from is taken as the previous release in the
  changelog. The update and tile state survive sign-out (B1 shows logged out too).
- B12: `CHANGELOG.md` is imported into the frontend bundle as text (Vite `?raw`) and rendered with
  `react-markdown` (no `dangerouslySetInnerHTML`). The tab is reached like Debug: not in the
  sidebar's tab list, only from the menu and the tile. GitHub's release page was rejected: it opens
  a browser and starts with signing and install steps that don't apply after an in-app update.

**Client changes** (all in upstream files under `tauri/src/`; small, low rebase risk):
- `lib/auto-update.ts`: `pollUpdates` only runs `check()` and stores the available version. Remove
  the background download, `isIdle` and the auto-install. `installUpdate` wraps the click (B2, B5).
- `lib/after-update.ts` (new): `handleAppStart`, called once from `app.tsx`, reads the B10/B11
  state; `openWhatsNew` opens the B12 tab. `lib/semver.ts` (new) compares versions.
- `store/store.ts`: `updateVersion` replaces `needsUpdate`; the "What's new" tile and tab state;
  `reset()` keeps the update state.
- `update.ts`: delete `downloadUpdateInBackground`, `installAndRelaunch`, `hasPendingUpdate` and
  `pendingUpdate`. Keep `downloadAndRelaunch` and report progress to the caller. It resolves
  `"no-update"` if `check()` finds none; `installUpdate` then hides the tile and logs a warning, with
  no toast. Every update `check()` returns is closed after use (the plugin never frees it), and both
  requests have timeouts (30 s check, 15 min download).
- `components/sidebar/Sidebar.tsx`: the update tile (B1–B5), the "What's new" tile (B11), and the
  "What's new" item in the avatar menu (B12).
- `windows/main-window/tabs/WhatsNew.tsx` (new) and `lib/changelog.ts` (new): the B12 tab and the
  `CHANGELOG.md` parser.
- `windows/settings/main.tsx`: remove the "Automatic updates" row (B9). The Rust
  `auto_update_enabled` setting and `set_auto_update_enabled` command stay, unused. Removing them
  would mean touching the `app_state.json` format for no gain.
- Logging to `hopp.log`: the frontend had no log bridge, and `console.error` reaches only the
  webview console. Add `@tauri-apps/plugin-log` on the JS side (the Rust plugin is already
  registered) with its capability, wrapped by `lib/log.ts` (new).
- Rust (B10 only): `AppData::main_window_placed` and `setup_tray_icon` in `lib.rs`, and the
  `show_main_window_when_placed` command in `main.rs` (typed in `core_payloads.ts`).
  `tauri_plugin_updater::Builder::new()` already verifies the signature against `pubkey`.
- `app.tsx`: B4 was half broken: the `incoming_call` listener, registered once, read a stale
  `updateInProgress` from the first render, so calls were never rejected during an update (invites
  were). It now reads the store.
- With `endpoints: []`, `check()` fails at once with "Updater does not have any endpoints set."
  without a request; polling treats that as "updater off" and stops, instead of logging every 15
  minutes.

## Tasks

- [x] Agree on Behavior
- [x] Generate the minisign keypair, store the private key and password, commit the public key
- [x] `tauri.conf.dataico-updater.json`, the conditional `--config` and `DATAICO_UPDATER_ENDPOINT` in `build-macos.sh`
- [x] `build-macos.sh`: tarball, `.sig` and `latest.json` (notes from a file) after the final staple, only for builds that have the updater on
- [x] `release.sh` and the `release` task (B13–B18), plus `gh` in `mise.toml`
- [x] `CHANGELOG.md` seeded with 1.0.32–1.0.34
- [x] Client: check-only polling, the update tile with progress and error handling, disabled in calls, toggle removed, logging
- [x] Client: post-update toast, "What's new" tile, menu item and tab from the bundled `CHANGELOG.md`
- [x] Local rehearsal: unsigned builds against a local feed (see notes)
- [ ] End-to-end test against a test feed (see Verification)
- [ ] Ship the first updater-enabled release with the task. Its notes say "install by hand one last time"
- [x] `DATAICO.md`: replace the "Auto-updater: off" row and the "How the updater is disabled" section, add a row for the release task, changelog and "What's new", and describe releasing in "Build"
- [ ] Preserve discoveries from `workingcontext.md` in `docs/specs/0004-auto-updater/notes.md` and link
      below before marking complete, merging or removing the worktree. Include notes, quirks,
      awkward behavior, gotchas and bugs (resolved vs. remaining); explicitly say if none were found.

## Verification

Automated: `yarn tsc --noEmit --skipLibCheck` in `tauri/` and `yarn prettier` from the repo root
(prettier is a root dependency); `shellcheck` on
`release.sh` and `build-macos.sh`.

Release task:
1. B13: each refusal, one at a time: wrong branch, dirty tree, behind origin, version not higher,
   existing tag, a missing credential, `gh` logged out.
2. B14: on a checkout with `dataico-v1.0.33` as the last tag, the draft lists the 22 commits above,
   minus the dropped prefixes.
3. B18: `DRY_RUN=1` stops after the build with the release commit local. Kill the task during the
   build, rerun it, and check it doesn't ask for notes or commit again.
4. B15–B17: the first real release. Check the commit, tag, `CHANGELOG.md`, the release body, the
   four files, and that `releases/latest/download/latest.json` serves the new version.

App, by someone with the signing setup. To avoid testing against the live feed, set
`DATAICO_UPDATER_ENDPOINT` to a prerelease, e.g.
`.../releases/download/dataico-updater-test/latest.json`:
1. Build and install version N with the test endpoint. Publish N+1 to the test prerelease.
2. B1: the tile appears within 15 minutes (or at relaunch), labeled "Update", with the version in
   its tooltip, logged in and logged out. No download happens until you click (check Activity
   Monitor › Network).
3. B2/B7: click it. The ring fills, then "Restarting". The app relaunches as N+1, and `hopp_core`
   was replaced too (core version in the log). You're still logged in and settings are unchanged.
   Screen sharing and remote control work without new permission prompts. Repeat once in each
   window style (Menu bar, Floating, Regular).
4. B10: after each relaunch in step 3, the main window opens once (under the tray icon in the Menu
   bar style) with "Hopp updated to N+1". Quitting and relaunching by hand doesn't show it again.
   With AeroSpace running (§TILING-WM), start the update from a workspace other than 1 in each
   window style: after the relaunch AeroSpace stays on that workspace, and in the Menu bar style
   `aerospace list-windows --all` lists no Hopp window.
5. B11/B12: the "New" tile is there; nothing opened by itself. Click it: the "What's new" tab shows
   N+1 marked new and older versions muted, and the tile is gone. The avatar menu's "What's new"
   opens the same tab. Install N+2 by hand (not via the tile): the "New" tile shows, without B10's
   toast.
6. B3: during a call, the tile is grayed, ignores clicks and shows "Update after the call".
7. B5: turn off Wi-Fi and click. The tile returns to "Update", the toast shows, `hopp.log` has the
   error, and a call placed to you rings normally.
8. B6: publish a `latest.json` whose signature doesn't match, then click. Nothing is installed, and
   B5's error path runs.
9. B8: an ad-hoc `task -d packaging/dataico install` build makes no request to the feed.
10. B9: Settings shows no "Automatic updates" row.
11. As a non-admin macOS user with Hopp in `/Applications`: does the install fail, or ask for an
    admin password? Record the result here.

## Decisions

- **Q:** When do updates install? **A:** Only when the user clicks the update button. No background
  download, no auto-install, and the Settings toggle is removed. (2026-10-08)
- **Q:** Feed host? **A:** GitHub Releases on `awkay/hopp`, which is public. (2026-10-08)
- **Q:** How are releases made? **A:** A local `task -d packaging/dataico release`, run by whoever
  has the Developer ID certificate and the updater key. CI was rejected (it needs those as repo
  secrets). Today's flow is fully manual (no `release.yml` runs on the fork). (2026-10-08)
- **Q:** Changelog? **A:** `CHANGELOG.md` in the repo, written by the release task from a draft of
  the commits since the last release, which the releaser edits. The same text goes into the GitHub
  release. (2026-10-08)
- **Q:** Where and how does the update button look? **A:** Same sidebar slot, redesigned as a
  labeled, blue-tinted tile with real download progress and a tooltip that works while disabled.
  (2026-10-08)
- **Q:** Do users see the release notes? **A:** Yes, but only if they choose to: after an update a
  "What's new" tile and a permanent avatar-menu item open an in-app tab from the bundled
  `CHANGELOG.md`. Never opened automatically. The main window does reopen once after an in-app
  update, to confirm it. (2026-10-08)

## Completion notes

[Notes, quirks, awkward behavior, gotchas and bugs](0004-auto-updater/notes.md)

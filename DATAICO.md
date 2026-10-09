# Hopp for Dataico

This fork (`github.com/awkay/hopp`) builds a Dataico-branded macOS Hopp app that talks to
our self-hosted server. `main` = upstream `gethopp/hopp` + a thin packaging layer + a few
fork-only features (see below).

## What differs from upstream

| | Upstream | Dataico build | Why |
|---|---|---|---|
| Server | Hopp cloud | `hopp.apps.dataico.world` (`VITE_API_BASE_URL`) | our self-hosted backend |
| Sidebar "Profile" | opens the configured server's home page (`Constants.webAppUrl`); before upstream #393 it opened `pair.gethopp.app` (Hopp cloud) | opens `/settings` on that server | `/settings` is the profile page; the home page is the dashboard |
| Bundle ID | `com.hopp.app` | `com.dataico.hopp` | separate identity, keychain, TCC grants, signing |
| Updates | checks `github.com/gethopp/hopp/releases/.../latest.json`; on macOS downloads in the background and installs and relaunches when the window is unfocused (Settings toggle "Automatic updates") | our own feed (`github.com/awkay/hopp/releases/latest/download/latest.json`, signed with our updater key) and click to update only: a blue "Update" tile in the sidebar shows download progress, is disabled during call activity ("Update after the call"), and on failure shows a toast and lets you retry. No Settings toggle. After an in-app update the main window opens once with "Hopp updated to X" (in the menu bar style once `setup_tray_icon` has placed the popup under the tray icon: `AppData::main_window_placed`, `show_main_window_when_placed`); after any upgrade a "New" tile (7 days or until opened) and the avatar menu's "What's new" open a tab with the bundled `CHANGELOG.md`. Incoming calls are rejected while an update downloads or installs (upstream's `incoming_call` listener read a stale `updateInProgress`, so only invites were), and the update state survives sign-out (upstream's `reset()` cleared it). Only notarized builds with the updater key check for updates (see "How in-app updates work") | upstream's feed would replace our build with the stock app; installing without asking interrupted people; manual zip installs left people on old builds |
| Releases | `release.yml` in CI; notes written by hand on GitHub | `task -d packaging/dataico release VERSION=x.y.z` on the releaser's Mac (notes draft from commits, `CHANGELOG.md`, version bump, notarized build with update files, atomic tag + push, GitHub release) | the fork never ran `release.yml`; releases were fully manual, and the updater needs signed update files and a `latest.json` |
| Window style (macOS) | menu bar app only: no Dock icon outside calls, borderless always-on-top window under the tray icon that hides on focus loss | Settings > Call settings > Window style, applied on the next launch (the settings window offers a restart): "Menu bar" (default, upstream behavior); "Floating window" (Dock icon and Cmd-Tab for the whole session, the same borderless fixed-size window, not always-on-top, dragged by its sidebar, remembers its position, Esc / Cmd-W hide it); "Regular window" (Dock icon and Cmd-Tab, normal titled resizable window). Both non-default styles: no tray positioning, no hide on focus loss, closing hides, Dock click reopens, optional menu bar icon | people who use Cmd-Tab or a window manager (e.g. AeroSpace, which floats the borderless window and tiles the regular one) could not treat Hopp as a normal app |
| Telemetry | Sentry / PostHog keys from CI | all empty | no data to upstream's accounts |
| Settings file | no way to find it from the app | "Show settings file in Finder" link at the bottom of Settings (`reveal_settings_file` command, path from `AppState::file_path`; reveals the folder if the file is missing) | the local state file (`app_state.json`, `app_state_<suffix>.json` in dev builds) is hard to find when debugging |
| Features | upstream releases | fork `main`, which carries typing text while drawing (PR #306), low-bandwidth mode, screen effects, "drawing persists" as the default draw mode, double-click while drawing plays the pointer click animation, and the call-end CPU fix (core stops the screen picker, drawing redraw thread and late camera/share windows when a call ends) | available to us before an upstream release; low-bandwidth mode and screen effects are fork-only |
| LiveKit Rust SDK | `gethopp/rust-sdks`, branch `hopp` | `awkay/rust-sdks`, branch `hopp-encoding-params` | adds `LocalVideoTrack::set_encoding_parameters`, which low-bandwidth mode needs |
| Tauri ↔ core IPC | one `Mutex<AppData>` held across send + 10 s wait; replies matched by variant | `CoreClient`: one ordered non-blocking queue, request ids, pipelined async requests, call ids on call messages, `AppData` split into small locks/atomics, no locks or waits on the main thread (see `docs/ipc.md`) | fixes "Hopp is not responding" hangs/deadlocks, late call-end messages ending the next call, stale responses, preferred camera ignored |
| Core restart (exit code 2) | swaps the socket, re-sends only the LiveKit URL | re-sends the full startup config (App Veil included), rebinds events, ends the call in the UI, backs off (max 3 per 10 min) | the old path silently dropped App Veil and other settings |
| Websocket handler shutdown | Redis loop sends on `done`, which the read loop closes | only the read loop closes `done`; the Redis loop cancels the context and the handler waits on either | upstream panics with "send on closed channel" when a client disconnects at the wrong moment, crashing the server and dropping everyone |
| Web app bundle / backend image | one self-contained HTML file with JS and images inlined: 7.8 MB with PNGs, 3.0 MB since upstream #389 switched them to WebP (and turned on compression in `selfhost/Caddyfile`); `iparaskev/hopp-backend` | hashed JS/CSS/images under `/static/react/` (served cacheable by our Caddy), WebP images smaller than upstream's (1200 px login screen, 160 px buddy icon); image built by `.github/workflows/publish-dataico-backend.yml` to `ghcr.io/awkay/hopp-backend:dataico-<sha>` (arm64) | the web UI was unusable on slow links: the single file is downloaded in full on every cold load; ours is ~0.9 MB compressed once, then cached |
| Call connect on slow links | LiveKit default 5 s signal connect timeout, 30 s room setup | 20 s per signal attempt, 90 s setup; a new call cancels a stale in-flight connect | 3G TLS handshakes exceed 5 s, so calls never connected |
| Screen effects | none | viewers pick an animated sticker (wand button in the screen-share window); it plays centred over the shared screen on the sharer's overlay and every viewer's window, one at a time (triggers while one plays are dropped). Lossy data topic `effect`, id only; assets compiled into core and validated at build time (`core/resources/effects/effects.md`) | fun reactions without covering the call in chat; old clients just log and ignore the packet |
| Team admins | only the team creator (or someone removed into a solo team) is admin; no way to make another member admin | admins grant/revoke admin for teammates from the Teammates page (shield button, confirm dialog); `PUT /api/auth/teammates/{userId}/admin` `{"is_admin": bool}`; revoking the team's last admin (including yourself) is refused with 400 | teams need more than one person who can manage members |
| macOS drawing window / menu-bar popup | drawing window sized with `set_maximized` (`NSWindow zoom:`, a toggle); menu-bar popup keeps Tauri's default style bits | drawing window gets an explicit frame covering the shared monitor; the popup's style mask is set to plain borderless at startup | zoom alternated full screen / 1x1, so local drawing failed every other time; any style bit (Tauri sets miniaturizable and full-size-content-view) makes the popup report itself as a standard window, so while Hopp is a Dock app (in a call) AeroSpace adopted it into a workspace and the menu-bar icon switched workspaces |
| macOS menu bar while sharing | only the Hopp icon | while you share your screen, the Hopp menu-bar item widens to three icons side by side, [draw] [stop sharing] [Hopp], each clickable: draw toggles local drawing with the saved "persist" setting (scribble icon while drawing, cursor when not), stop ends the share, Hopp opens the popup. One item (icon swap, click split in thirds), so the icons stay together where the Hopp icon is; shown from core's participants snapshot, back to the Hopp icon when sharing or the call ends (`tray.rs`). Settings › Call settings › "Show sharing buttons in menu bar" (on by default) turns it off |
| Esc in the menu bar popup | does nothing | hides it, like the floating window style (same rules: not while a menu, dialog or text field uses Esc) | close the popup from the keyboard |
| Favorite teammates | none | star a teammate (star on row hover, filled when starred) to pin them in a Favorites section above Online/Offline (online first); user IDs stored locally in `app_state.json` (`favorite_teammates`), no backend change; IDs that are no longer teammates are pruned after each successful teammates fetch | quick access to the people you pair with most, like Tuple |
| CI and release builds | CI clippy and release builds for `core/` and `tauri/`, and `release.yml` app builds, on Windows, Intel and Apple Silicon macOS | Apple Silicon macOS only (`aarch64-apple-darwin`) | we only ship the Apple Silicon macOS app, so the other targets only cost CI time |
| Rust toolchain in CI | clippy, tests and builds pinned to 1.96.1 | latest stable (`dtolnay/rust-toolchain@stable`), like `release.yml` already was | local rustup stable and CI run the same clippy, and there is no pin to bump |
| Tests | CI runs only the Go integration tests; the `core/tests` harness speaks upstream's IPC and its scenarios need a person watching the screen | CI also runs the core, `socket_lib` and Tauri unit tests, all Go tests (`internal/` too), and builds the harness, and checks that each `@tauri-apps/*` npm package matches its crate's major.minor (`scripts/check-tauri-versions.mjs`, which `tauri build` would otherwise reject only at release time); the harness speaks our request-id / call-id IPC (`core/tests/src/ipc.rs`) and has self-checking smoke scenarios run locally with `core/tests/smoke.sh`, optionally over an emulated network (`core/tests/netem.sh`) | the Rust tests ran nowhere, and the harness stopped compiling with the IPC rewrite without anyone noticing |
| `docs/` | Astro/Starlight source of the user docs site docs.gethopp.app, a Yarn workspace | deleted; `docs/` holds our own docs (`docs/ipc.md`), feature specs (`docs/specs/`) and work tracker (`docs/TRACKER.md`). README images moved to `banner.png` (an identical copy already at the root) and `.github/readme/`. In-app docs links still go to docs.gethopp.app | nobody here built or deployed the site, and installing it slowed every `yarn install`; we needed docs for ourselves and for agents |
| Agent instructions | `AGENTS.md` at the root and in `core/`, which the `CLAUDE.md` files import or link to. Agents may run only `task build_dev`, never `cargo fmt` / `clippy`, and follow a generic plan-mode / `tasks/todo.md` workflow | `CLAUDE.md` at the root and in `core/`, no `AGENTS.md`. Agents run the checks CI runs (build, unit tests, clippy, fmt, typecheck, lint, Go tests, core smoke scenarios) and never start the app, dev servers or installs. Plans and status live in `docs/` | upstream's files listed backend tasks that don't exist, contradicted each other and CI, and left agents no way to check their own work |
| `task backend:compose-up` | runs `backend/docker-files/docker-compose.yml`, which upstream #305 moved to `backend/dev-compose.yml`, so the task fails | runs `backend/dev-compose.yml` (Postgres 16 and Redis) | local setup needed a hand-typed `docker compose` command |

The packaging layer lives in new files; it modifies no upstream file:

- `tauri/src-tauri/tauri.conf.dataico.json` - overlay merged via `tauri build --config`
  (identifier, release sidecar path, no updater artifacts, `plugins.updater.endpoints: []`).
- `packaging/dataico/build-macos.sh` - the one build script.
- `packaging/dataico/.env.example` - signing, notarization and updater-key variables, read by
  `build-macos.sh` and `release.sh` (copy to `.env`, git-ignored).
- `tauri/src-tauri/tauri.conf.dataico-updater.json` - the updater's `pubkey` and our feed, added
  as a second `--config` only to notarized builds that have the updater key.
- `packaging/dataico/release.sh` - the release task (spec 0004, B13–B18).
- `packaging/dataico/lib.sh` - sourced by both scripts: the `.env` loader, logging, notarization
  mode, the repo, tag, host and file names, and the `build-info.json` check.
- `CHANGELOG.md` - user-facing notes per release, written by the release task and bundled into the
  app for "What's new".
- `packaging/dataico/Taskfile.yml` - `build`, `install` and `release` tasks, run as
  `task -d packaging/dataico <task>`. Deliberately not included from the root `Taskfile.yml`:
  Task would hand its root `dotenv` (`packaging/.env`, `tauri/.env`) to the build, and a dev
  `VITE_API_BASE_URL` there would override the release server.
- `scripts/install-macos.sh` - installs the last build to `/Applications`; the `install` task runs
  it after `build-macos.sh` (see "Installing").

**Fork-only features modify upstream files.** Low-bandwidth mode changes `core/` (new
`core/src/bandwidth_mode.rs`, plus `room_service.rs`, `lib.rs`, `snapshot_sender.rs`,
`socket_lib`), `tauri/` (settings, commands, sidebar and in-call turtle buttons), and switches the
`livekit` dependency in `core/Cargo.toml` / `core/Cargo.lock` to `awkay/rust-sdks`. Screen effects add
`core/src/effects*`, `core/src/graphics/effect_renderer.rs`, `core/resources/effects/` and a
`core/build.rs` step, and change `graphics_context.rs`, `screensharing_window.rs`,
`room_service.rs`, `lib.rs` and `window_manager.rs` (core only). Click-to-update and "What's new"
change `tauri/src/update.ts`, `lib/auto-update.ts`, `store/store.ts`, `components/sidebar/Sidebar.tsx`,
`windows/main-window/app.tsx`, `windows/settings/main.tsx`, `core_payloads.ts`, `tauri/package.json`
(`@tauri-apps/plugin-log`, `react-markdown`), `src-tauri/capabilities/desktop.json` (`log:default`)
and `src-tauri/src/{lib,main}.rs` (`show_main_window_when_placed`), and add `lib/after-update.ts`,
`lib/changelog.ts`, `lib/log.ts`, `lib/semver.ts` and `windows/main-window/tabs/WhatsNew.tsx`.

**Low-bandwidth mode** (client-only; no backend or LiveKit server changes). The turtle button
during a call asks for low bandwidth; the screen share then drops to 1080p / 15 fps / 900 kbps
without restarting, and cameras drop to their lowest quality. It stays on while anyone in the call
asks for it. The turtle in the sidebar makes every call you join start with it requested. Everyone
in the call needs this build: an older sharer ignores the request.

**Screen effects** (client-only; no backend or LiveKit server changes). A viewer clicks the wand
button in the screen-share window header and picks an effect. It plays centred over the shared
screen, on the sharer's overlay and in every viewer's window, and is never part of the video. Only
one plays at a time; triggers that arrive while one is playing are dropped. Effects are animated
WebP files compiled into core and listed in `core/resources/effects/effects.toml`. **To add or change
an effect, read `core/resources/effects/effects.md`**: manifest fields, size and length limits,
export recipes, and what the build checks. A GPU error turns effects off in that window for the rest
of the call instead of crashing core. Everyone needs this build to see effects; older clients log
and ignore them.

**How in-app updates work** (spec `docs/specs/0004-auto-updater.md`): `tauri.conf.dataico.json`
keeps `plugins.updater.endpoints: []`, so ad-hoc, signed-only and dev builds never check:
`check()` fails at once with "Updater does not have any endpoints set." before any request, and
`pollUpdates` stops polling. `build-macos.sh` adds `tauri.conf.dataico-updater.json` (our `pubkey`
and feed) only when the build is notarized, `TAURI_SIGNING_PRIVATE_KEY` is set and the overlay's
`pubkey` isn't empty; otherwise it warns and builds with the updater off. The updater plugin
installs only an update whose signature matches the built-in `pubkey`. The signature covers only the
tarball, not the `version` in `latest.json`, so whoever can edit our GitHub releases can serve any
build we ever signed (an older one, or a test build) as "newer". Our key stops anyone else; after a
relaunch into a version other than the one offered, `hopp.log` warns. Updates keep TCC grants
because every release has the same Developer ID team and bundle ID. **If the updater private key is
lost, no installed app can be updated again**: everyone would reinstall by hand a build with a new
`pubkey`. The private key and its password live in the Brazil team's shared Bitwarden.

**Known upstream hardcodings of `com.hopp.app`** (not patched, so packaging stays in new files):
- "Report issue" -> copy logs reads `~/Library/Logs/com.hopp.app/hopp.log`; our logs are in
  `~/Library/Logs/com.dataico.hopp/`.
- The app's own bundle may appear in the "hide applications while sharing" picker.

## Prerequisites

- macOS on Apple Silicon (arm64). Intel (x86_64) should work but is untested.
- Xcode Command Line Tools: `xcode-select --install`
- Rust via rustup (`https://rustup.rs`, or `brew install rustup && rustup-init`).
- Node.js **20** (`.nvmrc`). Newer Node (e.g. 26) breaks the pinned yarn 4.9.2
  (`onCancel` error). `nvm install 20`, `mise install node@20` or `brew install node@20`; the script finds any of them.
- Optional: `LK_CUSTOM_WEBRTC=/path/to/prebuilt/libwebrtc`. Without it, the first core build
  downloads libwebrtc (several hundred MB) from `gethopp/rust-sdks` releases; the SDK fork does
  not host its own copy. `cargo --offline` does not prevent this download (the `webrtc-sys` build
  script fetches it itself).

## Build

```bash
git clone git@github.com:awkay/hopp.git && cd hopp
packaging/dataico/build-macos.sh   # or: task -d packaging/dataico build
```

Output: `dist/dataico/Hopp-Dataico-<version>-<sha>-<arch>.zip` (the `.app` is `hopp.app`).

- **Unsigned** (no Apple variables set): ad-hoc signed. Good for the builder's own Mac only.
  Screen Recording/Accessibility grants reset on every rebuild.
- **Signed + notarized**: set the variables below (shell or `packaging/dataico/.env`) and run the
  same command. The script fails early if the identity isn't in your keychain, then verifies
  with `codesign --verify --deep --strict`, `spctl -a -t exec`, and `xcrun stapler validate`,
  and checks the bundled `hopp_core` sidecar has the same Team ID and hardened runtime.

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Dataico ... (TEAMID)"
# preferred notarization credentials
export APPLE_API_ISSUER=<issuer uuid> APPLE_API_KEY=<key id> APPLE_API_KEY_PATH=/path/AuthKey_<key id>.p8
# or: export APPLE_ID=... APPLE_PASSWORD=<app-specific password> APPLE_TEAM_ID=TEAMID
packaging/dataico/build-macos.sh
```

Identity set but no notarization credentials -> signed only; Gatekeeper still blocks it on other Macs.

Other overrides: `VITE_API_BASE_URL=<host>` for a different server.

**Update files.** With the updater key also set (`TAURI_SIGNING_PRIVATE_KEY`, the key file's
contents or its path, and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`), a notarized build also writes
`hopp_aarch64.app.tar.gz`, its `.sig` and `latest.json` to `dist/dataico/`.
`DATAICO_RELEASE_NOTES=<file>` fills `latest.json`'s notes. To test updates without the live feed,
set `DATAICO_UPDATER_ENDPOINT` to a prerelease's `latest.json` (e.g.
`.../releases/download/dataico-updater-test/latest.json`): the app checks that feed, and
`latest.json` points to the tarball next to it, so upload both to that prerelease.

The last file a build writes is `dist/dataico/build-info.json`: version, full commit, dirty flag,
server, update feed, notes hash, Team ID and whether it was notarized. A build deletes it first,
and a failed or interrupted build removes the files it had started writing in `dist/dataico/`.

## Releasing

```bash
task -d packaging/dataico release VERSION=1.0.35            # DRY_RUN=1 stops after the build
```

Run it on `main`, clean and level with `origin/main`, with the Apple variables above, the updater
key and a logged-in `gh` (`brew install gh`; `mise install` of `gh` can fail on older mise with
"GitHub attestations verification failed"). It refuses to start, listing every problem, otherwise.
It drafts the notes from the commits since the last `dataico-v*` release (by patch content, so it
survives rebases; housekeeping commits dropped) and opens them in `$EDITOR`: rewrite them as one
bullet per user-visible change, bold lead. It stops if they come back empty or unedited (the draft
is kept for the rerun). Then it bumps `tauri.conf.json`, adds the notes to
`CHANGELOG.md`, commits `chore(release): Dataico <VERSION>`, builds, tags, pushes `main` and the tag
atomically, uploads a draft GitHub release and publishes it as latest, which is what the app's feed
serves. If a step fails, fix it and rerun with the same `VERSION`: it continues where it stopped
without asking for notes or committing again. A rerun reuses the build in `dist/dataico/` only if
its `build-info.json` matches the release (this commit, a clean tree, the default server, our feed,
updater on, the Developer ID team, notarized, these notes); otherwise it says why and rebuilds.
`VERSION` is `x.y.z` without leading zeros. The task never exports the signing secrets: it only
checks that they are set, and only `build-macos.sh` receives them.

## One-time Apple setup

1. **Developer ID Application certificate.** Only the Account Holder, or an Admin who has been
   granted Developer ID access, can create it (developer.apple.com -> Certificates -> "+" ->
   Developer ID Application, G2 Sub-CA). Create the CSR with Keychain Access on the Mac that will
   sign, download the `.cer`, and double-click it so it lands in the **login** keychain next to its
   private key. Check: `security find-identity -v -p codesigning` lists
   `"Developer ID Application: ... (TEAMID)"`. To sign on another Mac, export the cert **with
   private key** as `.p12` and import it there.
2. **Notarization key.** App Store Connect -> Users and Access -> Integrations -> App Store
   Connect API -> Team Keys -> generate a key (Developer role is enough). Note the Issuer ID and
   Key ID; download `AuthKey_<KEYID>.p8` (downloadable once), keep it outside the repo.
3. **Updater key** (once for the team, not per releaser). From `tauri/`:
   `yarn tauri signer generate -w ~/.tauri/hopp-dataico.key`, with a password. Put the private
   key file's contents and the password in the Brazil team's shared Bitwarden, and commit the printed
   public key as `pubkey` in `tauri/src-tauri/tauri.conf.dataico-updater.json`. Releasers set
   `TAURI_SIGNING_PRIVATE_KEY` (contents or path) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
   Never regenerate it while installed apps carry the old `pubkey`: they could not update again.

## Installing

From a clone (macOS, Task via `brew install go-task`): `task -d packaging/dataico install` builds
the current checkout, quits a running Hopp, replaces `/Applications/hopp.app` and opens it. It refuses to overwrite an
`/Applications/hopp.app` that isn't `com.dataico.hopp` (the official app).

From a zip: unzip, drag `hopp.app` to `/Applications`, open it, and grant Screen Recording, Accessibility,
Camera and Microphone when prompted. For an unsigned build on another Mac:
`xattr -dr com.apple.quarantine /Applications/hopp.app`.

**Don't install the official Hopp app on the same Mac.** Both register the `hopp://` URL scheme
(the server's login page redirects to `hopp://`), so login may open the wrong app. Both are also
named `hopp.app`.

## Keeping in sync with upstream

```bash
git remote add upstream https://github.com/gethopp/hopp.git   # once
git fetch upstream
git checkout main
git rebase upstream/main
git push --force-with-lease origin main
packaging/dataico/build-macos.sh
```

The packaging commit only adds files and rebases cleanly. The fork-only feature commits edit
upstream files, so the rebase can stop with conflicts. Likely spots are the IPC layer (`core/socket_lib/src/*`, `tauri/src-tauri/src/{lib,main,core_client}.rs`), screen effects (`core/src/window/screensharing_window.rs`, `core/src/graphics/graphics_context.rs`, `core/build.rs`), and for low-bandwidth mode
`core/src/room_service.rs`, `core/src/lib.rs`, `core/socket_lib/src/lib.rs`,
`tauri/src-tauri/src/main.rs`, `tauri/src/store/store.ts`,
`tauri/src/components/ui/call-center.tsx` and `tauri/src/components/sidebar/Sidebar.tsx`. The
updater client touches `tauri/src/update.ts`, `tauri/src/lib/auto-update.ts`, `store.ts`,
`Sidebar.tsx` and `tauri/src/windows/main-window/app.tsx`. On a conflict on `tauri.conf.json`
`version`, keep ours: every release must be higher than the last for the updater.

**If upstream changed its LiveKit SDK** (a conflict on the `livekit` line in `core/Cargo.toml`, or
in `core/Cargo.lock`), the SDK fork has to follow before main can build:

1. Resolve the rebase: keep our `livekit = { git = "https://github.com/awkay/rust-sdks", ... }`
   line in `core/Cargo.toml`, and take upstream's lockfile for now
   (`git checkout <upstream commit> -- core/Cargo.lock`).
2. Rebase the SDK fork onto upstream's SDK branch. Tag the old commit first, so older fork
   commits whose `Cargo.lock` pins it still build after the force-push:

   ```bash
   # one-time clone; the repo uses Git LFS for example media we don't need
   GIT_LFS_SKIP_SMUDGE=1 git clone git@github.com:awkay/rust-sdks.git
   cd rust-sdks && git remote add upstream https://github.com/gethopp/rust-sdks.git

   git fetch upstream
   git switch hopp-encoding-params
   t="hopp-encoding-params-$(git rev-parse --short HEAD)"
   git tag "$t" && git push origin "$t"
   git rebase upstream/hopp      # one small commit on livekit/src/room/{options.rs,track/local_video_track.rs}
   git push --force-with-lease origin hopp-encoding-params
   ```
3. Re-pin and continue: `cd core && cargo update -p livekit`, then build, `git add` the
   lockfile, and `git rebase --continue`.

**Upstream's docs site is deleted here**, and upstream still edits it. When the rebase replays
that deletion and upstream changed a docs file, it stops with a modify/delete conflict. Files
upstream added under `docs/` survive the rebase without a conflict. In both cases, run the
command below: during the conflict, then `git rebase --continue`; after the rebase, if it deletes
anything, commit the deletion. Our own docs (`docs/*.md`, `docs/specs/`) aren't touched.

```bash
git rm -rq --ignore-unmatch docs/src docs/public docs/package.json docs/astro.config.mjs \
  docs/tsconfig.json docs/.gitignore
```

A conflict in `yarn.lock` (upstream bumped a package we dropped with the docs workspace): take
the rebased side (`git checkout --ours yarn.lock`; in a rebase, "ours" is upstream plus the
commits already replayed), run `yarn install --mode=update-lockfile` (Node 20), then `git add yarn.lock`.

Others then update with `git fetch && git reset --hard origin/main` (main is rebased, not merged).

If `tauri.conf.json` changes the sidecar path, updater config, or bundle layout, update
`tauri.conf.dataico.json` accordingly.

## Contributing upstream

Branch PRs for `gethopp/hopp` from `upstream/main`, not from fork `main`
(`git switch -c my-fix upstream/main`), so the Dataico packaging and fork-only feature commits
don't leak into them. Low-bandwidth mode can't go upstream as-is: it needs
`set_encoding_parameters` merged into `gethopp/rust-sdks` first, and `core/Cargo.toml` pointed
back at it.

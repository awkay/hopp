# Hopp for Dataico

This fork (`github.com/awkay/hopp`) builds a Dataico-branded macOS Hopp app that talks to
our self-hosted server. `main` = upstream `gethopp/hopp` + a thin packaging layer + a few
fork-only features (see below).

## What differs from upstream

| | Upstream | Dataico build | Why |
|---|---|---|---|
| Server | Hopp cloud | `hopp.apps.dataico.world` (`VITE_API_BASE_URL`) | our self-hosted backend |
| Sidebar "Profile" | opens `pair.gethopp.app` (Hopp cloud) | opens `/settings` on our server (`Constants.webAppUrl`, so a custom server URL is respected) | our accounts live on our server, and `/settings` is the profile page |
| Bundle ID | `com.hopp.app` | `com.dataico.hopp` | separate identity, keychain, TCC grants, signing |
| Auto-updater | checks `github.com/gethopp/hopp/releases/.../latest.json` | off | upstream updates would replace our build with the stock app |
| Window style (macOS) | menu bar app only: no Dock icon outside calls, borderless always-on-top window under the tray icon that hides on focus loss | Settings > Call settings > Window style, applied on the next launch (the settings window offers a restart): "Menu bar" (default, upstream behavior); "Floating window" (Dock icon and Cmd-Tab for the whole session, the same borderless fixed-size window, not always-on-top, dragged by its sidebar, remembers its position, Esc / Cmd-W hide it); "Regular window" (Dock icon and Cmd-Tab, normal titled resizable window). Both non-default styles: no tray positioning, no hide on focus loss, closing hides, Dock click reopens, optional menu bar icon | people who use Cmd-Tab or a window manager (e.g. AeroSpace, which floats the borderless window and tiles the regular one) could not treat Hopp as a normal app |
| Telemetry | Sentry / PostHog keys from CI | all empty | no data to upstream's accounts |
| Features | upstream releases | fork `main`, which carries typing text while drawing (PR #306), low-bandwidth mode, screen effects, "drawing persists" as the default draw mode, double-click while drawing plays the pointer click animation, and the call-end CPU fix (core stops the screen picker, drawing redraw thread and late camera/share windows when a call ends) | available to us before an upstream release; low-bandwidth mode and screen effects are fork-only |
| LiveKit Rust SDK | `gethopp/rust-sdks`, branch `hopp` | `awkay/rust-sdks`, branch `hopp-encoding-params` | adds `LocalVideoTrack::set_encoding_parameters`, which low-bandwidth mode needs |
| Tauri ↔ core IPC | one `Mutex<AppData>` held across send + 10 s wait; replies matched by variant | `CoreClient`: one ordered non-blocking queue, request ids, pipelined async requests, call ids on call messages, `AppData` split into small locks/atomics, no locks or waits on the main thread (see `AGENTS.md` "Protocol rules") | fixes "Hopp is not responding" hangs/deadlocks, late call-end messages ending the next call, stale responses, preferred camera ignored |
| Core restart (exit code 2) | swaps the socket, re-sends only the LiveKit URL | re-sends the full startup config (App Veil included), rebinds events, ends the call in the UI, backs off (max 3 per 10 min) | the old path silently dropped App Veil and other settings |
| Websocket handler shutdown | Redis loop sends on `done`, which the read loop closes | only the read loop closes `done`; the Redis loop cancels the context and the handler waits on either | upstream panics with "send on closed channel" when a client disconnects at the wrong moment, crashing the server and dropping everyone |
| Web app bundle / backend image | one 7.8 MB self-contained HTML file (JS and 2.5 MB + 1.7 MB PNGs inlined), `iparaskev/hopp-backend` | hashed JS/CSS/images under `/static/react/` (served cacheable by our Caddy), images resized to WebP; image built by `.github/workflows/publish-dataico-backend.yml` to `ghcr.io/awkay/hopp-backend:dataico-<sha>` (arm64) | the web UI was unusable on slow links: ~7.8 MB on every cold load, now ~0.9 MB compressed once, then cached |
| Call connect on slow links | LiveKit default 5 s signal connect timeout, 30 s room setup | 20 s per signal attempt, 90 s setup; a new call cancels a stale in-flight connect | 3G TLS handshakes exceed 5 s, so calls never connected |
| Screen effects | none | viewers pick an animated sticker (wand button in the screen-share window); it plays centred over the shared screen on the sharer's overlay and every viewer's window, one at a time (triggers while one plays are dropped). Lossy data topic `effect`, id only; assets compiled into core and validated at build time (`core/resources/effects/effects.md`) | fun reactions without covering the call in chat; old clients just log and ignore the packet |
| Team admins | only the team creator (or someone removed into a solo team) is admin; no way to make another member admin | admins grant/revoke admin for teammates from the Teammates page (shield button, confirm dialog); `PUT /api/auth/teammates/{userId}/admin` `{"is_admin": bool}`; revoking the team's last admin (including yourself) is refused with 400 | teams need more than one person who can manage members |
| macOS drawing window / menu-bar popup | drawing window sized with `set_maximized` (`NSWindow zoom:`, a toggle); menu-bar popup keeps Tauri's default style bits | drawing window gets an explicit frame covering the shared monitor; the popup's style mask is set to plain borderless at startup | zoom alternated full screen / 1x1, so local drawing failed every other time; any style bit (Tauri sets miniaturizable and full-size-content-view) makes the popup report itself as a standard window, so while Hopp is a Dock app (in a call) AeroSpace adopted it into a workspace and the menu-bar icon switched workspaces |
| macOS menu bar while sharing | only the Hopp icon | while you share your screen, the Hopp menu-bar item widens to three icons side by side, [draw] [stop sharing] [Hopp], each clickable: draw toggles local drawing with the saved "persist" setting (scribble icon while drawing, cursor when not), stop ends the share, Hopp opens the popup. One item (icon swap, click split in thirds), so the icons stay together where the Hopp icon is; shown from core's participants snapshot, back to the Hopp icon when sharing or the call ends (`tray.rs`). Settings › Call settings › "Show sharing buttons in menu bar" (on by default) turns it off |
| Esc in the menu bar popup | does nothing | hides it, like the floating window style (same rules: not while a menu, dialog or text field uses Esc) | close the popup from the keyboard |
| Favorite teammates | none | star a teammate (star on row hover, filled when starred) to pin them in a Favorites section above Online/Offline (online first); user IDs stored locally in `app_state.json` (`favorite_teammates`), no backend change; IDs that are no longer teammates are pruned after each successful teammates fetch | quick access to the people you pair with most, like Tuple |
| CI and release builds | CI clippy and release builds for `core/` and `tauri/`, and `release.yml` app builds, on Windows, Intel and Apple Silicon macOS | Apple Silicon macOS only (`aarch64-apple-darwin`) | we only ship the Apple Silicon macOS app, so the other targets only cost CI time |

The packaging layer lives in new files; it modifies no upstream file:

- `tauri/src-tauri/tauri.conf.dataico.json` - overlay merged via `tauri build --config`
  (identifier, release sidecar path, no updater artifacts, `plugins.updater.endpoints: []`).
- `packaging/dataico/build-macos.sh` - the one build script.
- `packaging/dataico/.env.example` - signing/notarization variables (copy to `.env`, git-ignored).

**Fork-only features modify upstream files.** Low-bandwidth mode changes `core/` (new
`core/src/bandwidth_mode.rs`, plus `room_service.rs`, `lib.rs`, `snapshot_sender.rs`,
`socket_lib`), `tauri/` (settings, commands, sidebar and in-call turtle buttons), and switches the
`livekit` dependency in `core/Cargo.toml` / `core/Cargo.lock` to `awkay/rust-sdks`. Screen effects add
`core/src/effects*`, `core/src/graphics/effect_renderer.rs`, `core/resources/effects/` and a
`core/build.rs` step, and change `graphics_context.rs`, `screensharing_window.rs`,
`room_service.rs`, `lib.rs` and `window_manager.rs` (core only).

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

**How the updater is disabled:** with an empty endpoint list, the updater plugin's `check()`
fails immediately with `EmptyEndpoints` before any network request. The frontend's only caller
(`tauri/src/lib/auto-update.ts`, `pollUpdates`) catches that, logs it to the console, and never
sets the "update available" flag, so no update button or error appears. The "Auto-update" toggle
in Settings is still shown but has no effect.

**Known upstream hardcodings of `com.hopp.app`** (not patched, so packaging stays in new files):
- "Report issue" -> copy logs reads `~/Library/Logs/com.hopp.app/hopp.log`; our logs are in
  `~/Library/Logs/com.dataico.hopp/`.
- The app's own bundle may appear in the "hide applications while sharing" picker.

## Prerequisites

- macOS on Apple Silicon (arm64). Intel (x86_64) should work but is untested.
- Xcode Command Line Tools: `xcode-select --install`
- Rust via rustup (`https://rustup.rs`, or `brew install rustup && rustup-init`).
- Node.js **20** (`.nvmrc`). Newer Node (e.g. 26) breaks the pinned yarn 4.9.2
  (`onCancel` error). `nvm install 20` or `brew install node@20`; the script finds either.
- Optional: `LK_CUSTOM_WEBRTC=/path/to/prebuilt/libwebrtc`. Without it, the first core build
  downloads libwebrtc (several hundred MB) from `gethopp/rust-sdks` releases; the SDK fork does
  not host its own copy. `cargo --offline` does not prevent this download (the `webrtc-sys` build
  script fetches it itself).

## Build

```bash
git clone git@github.com:awkay/hopp.git && cd hopp
packaging/dataico/build-macos.sh
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

## Installing

Unzip, drag `hopp.app` to `/Applications`, open it, and grant Screen Recording, Accessibility,
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
`tauri/src/components/ui/call-center.tsx` and `tauri/src/components/sidebar/Sidebar.tsx`.

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

Others then update with `git fetch && git reset --hard origin/main` (main is rebased, not merged).

If `tauri.conf.json` changes the sidecar path, updater config, or bundle layout, update
`tauri.conf.dataico.json` accordingly.

## Contributing upstream

Branch PRs for `gethopp/hopp` from `upstream/main`, not from fork `main`
(`git switch -c my-fix upstream/main`), so the Dataico packaging and fork-only feature commits
don't leak into them. Low-bandwidth mode can't go upstream as-is: it needs
`set_encoding_parameters` merged into `gethopp/rust-sdks` first, and `core/Cargo.toml` pointed
back at it.

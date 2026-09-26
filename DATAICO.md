# Hopp for Dataico

This fork (`github.com/awkay/hopp`) builds a Dataico-branded macOS Hopp app that talks to
our self-hosted server. `main` = upstream `gethopp/hopp` + a thin packaging layer.

## What differs from upstream

| | Upstream | Dataico build | Why |
|---|---|---|---|
| Server | Hopp cloud | `hopp.apps.dataico.world` (`VITE_API_BASE_URL`) | our self-hosted backend |
| Bundle ID | `com.hopp.app` | `com.dataico.hopp` | separate identity, keychain, TCC grants, signing |
| Auto-updater | checks `github.com/gethopp/hopp/releases/.../latest.json` | off | upstream updates would replace our build with the stock app |
| Telemetry | Sentry / PostHog keys from CI | all empty | no data to upstream's accounts |
| Features | upstream releases | fork `main`, which carries typing text while drawing (PR #306) | available to us before an upstream release |

All of this lives in new files; no upstream file is modified:

- `tauri/src-tauri/tauri.conf.dataico.json` - overlay merged via `tauri build --config`
  (identifier, release sidecar path, no updater artifacts, `plugins.updater.endpoints: []`).
- `packaging/dataico/build-macos.sh` - the one build script.
- `packaging/dataico/.env.example` - signing/notarization variables (copy to `.env`, git-ignored).

**How the updater is disabled:** with an empty endpoint list, the updater plugin's `check()`
fails immediately with `EmptyEndpoints` before any network request. The frontend's only caller
(`tauri/src/lib/auto-update.ts`, `pollUpdates`) catches that, logs it to the console, and never
sets the "update available" flag, so no update button or error appears. The "Auto-update" toggle
in Settings is still shown but has no effect.

**Known upstream hardcodings of `com.hopp.app`** (not patched, to keep the diff at zero):
- "Report issue" -> copy logs reads `~/Library/Logs/com.hopp.app/hopp.log`; our logs are in
  `~/Library/Logs/com.dataico.hopp/`.
- The app's own bundle may appear in the "hide applications while sharing" picker.
- Sidebar "Profile" opens `pair.gethopp.app`.

## Prerequisites

- macOS on Apple Silicon (arm64). Intel (x86_64) should work but is untested.
- Xcode Command Line Tools: `xcode-select --install`
- Rust via rustup (`https://rustup.rs`, or `brew install rustup && rustup-init`).
- Node.js **20** (`.nvmrc`). Newer Node (e.g. 26) breaks the pinned yarn 4.9.2
  (`onCancel` error). `nvm install 20` or `brew install node@20`; the script finds either.
- Optional: `LK_CUSTOM_WEBRTC=/path/to/prebuilt/libwebrtc`. Without it, the first core build
  downloads libwebrtc (several hundred MB).

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
git rebase upstream/main        # our commits are new files only, so this is normally conflict-free
git push --force-with-lease origin main
packaging/dataico/build-macos.sh
```

Others then update with `git fetch && git reset --hard origin/main` (main is rebased, not merged).

If `tauri.conf.json` changes the sidecar path, updater config, or bundle layout, update
`tauri.conf.dataico.json` accordingly.

## Contributing upstream

Branch PRs for `gethopp/hopp` from `upstream/main`, not from fork `main`
(`git switch -c my-fix upstream/main`), so the Dataico packaging commits don't leak into them.

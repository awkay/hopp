# Tracker

The only place status lives for features and work that spans sessions. Small fixes don't get a row.

- **ID**: next free four-digit number. Its spec, if any, is `specs/<ID>-<slug>.md`.
- **Status**: `idea` → `spec` → `ready` (user approved the spec) → `doing` → `done` (Notes: release)
  or `dropped` (Notes: why). Finished rows move to "Done".

## Active

| ID | Item | Status | Spec | Notes |
|---|---|---|---|---|
| 0001 | In-app docs links go to upstream's docs.gethopp.app: sidebar rooms link (`web-app/src/components/sidebar.tsx`), report window (`tauri/src/windows/main-window/report.tsx`), welcome email self-hosting link (`backend/web/emails/hopp-welcome.html`) | idea | | point at our own pages or remove |
| 0002 | "Report issue" → copy logs reads `~/Library/Logs/com.hopp.app/hopp.log`; our logs are in `com.dataico.hopp` | idea | | |
| 0003 | Automated testing: `core/tests` harness ported to the request-id / call-id IPC, self-checking smoke scenarios (`core/tests/smoke.sh`), network emulation (`core/tests/netem.sh`), Rust unit tests + all Go tests in CI | doing | | harness builds, socket_lib 35/35 and Tauri 11/11 pass locally. Next: user runs `smoke.sh` once (LiveKit + mic/screen grants), fix failures from `core/tests/out/`; `netem.sh` untested (needs root); core `cargo test --lib` and Go `./...` first run in CI |
| 0004 | In-app updates (click-to-update tile, post-update "What's new" from a bundled `CHANGELOG.md`) and a one-command release task | doing | [0004](specs/0004-auto-updater.md) | Behavior agreed 2026-10-08. Implemented in `spec/0004-auto-updater`; updater keypair generated 2026-10-08, then replaced the same day before any release shipped it by one made on the releasing Mac (key ID `A6F548CDEAA99EC1`; private key and password in that Mac's `packaging/dataico/.env` and the Brazil team's shared Bitwarden, public key in `tauri.conf.dataico-updater.json`). Local rehearsal with unsigned builds and a local feed passed 2026-10-08 (spec notes); it found and fixed a build-breaking plugin-log version mismatch. Pre-PR review fixes (build-info.json gate on release reruns, secrets not exported by `release.sh`, updater timeouts, CI plugin-version check) and squash-merged to `main` 2026-10-08. Next: `DRY_RUN=1` release rehearsal of the reworked scripts, signed end-to-end test against a test prerelease (B3, B7, B11 after a manual install, the update tile states at the fixed window size, Menu bar style under AeroSpace still unverified), first release |

## Done

| ID | Item | Status | Spec | Notes |
|---|---|---|---|---|

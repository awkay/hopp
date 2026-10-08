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

## Done

| ID | Item | Status | Spec | Notes |
|---|---|---|---|---|

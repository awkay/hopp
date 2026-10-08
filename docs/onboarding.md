# Onboarding

For a developer starting on this repo with an Apple Silicon Mac, the only platform we ship. Also
read root `CLAUDE.md` (fork policy, checks) and `DATAICO.md` (what we ship and how it differs from
upstream). Agents: skip setup and running, which need a person at a desktop; run only the checks in
`CLAUDE.md`.

## Setup

In this order:

1. **Xcode Command Line Tools:** `xcode-select --install`.
2. **Homebrew:** `brew install mise livekit mkcert pre-commit`, plus a Docker runtime (Docker
   Desktop, OrbStack or colima) for Postgres and Redis. `livekit` provides `livekit-server`.
   Optional: `livekit-cli` (the `lk` command, only for `task livekit:generate-user-token`) and
   `fswatch` (web app live reload).
3. **mise** installs node, go, task, golangci-lint and gh (only releases use it) at the versions in
   `mise.toml` (each line says why). Add `eval "$(mise activate zsh)"` to `~/.zshrc`, open a new
   shell, then from the repo root run `mise trust` and `mise install`. Check: `node -v` in the repo
   prints `v20.x`. If `mise install` fails on `gh` with "GitHub attestations verification failed"
   (seen with mise 2025.9.15), update mise or `brew install gh`.
4. **Yarn:** `corepack enable`, run in the repo so it installs into mise's Node 20. `yarn` then runs
   the Yarn 4.9.2 checked into `.yarn/releases/`.
5. **Rust** via rustup (https://rustup.rs), not mise or brew. CI uses the latest stable, so run
   `rustup update` now and then: an older clippy misses lints CI reports.
6. **JS dependencies:** `yarn install` at the repo root (workspaces `tauri/` and `web-app/`).
7. **Git hooks:** `pre-commit install`. They format staged Rust (`cargo fmt`) and TS (prettier) and
   lint Go. Without them, unformatted Rust is only caught by CI.
8. **Local HTTPS certs:** `cd backend && task create-certs`. `mkcert -install` adds a local CA to the
   system keychain (asks for your password) and the certs land in `backend/certs/`. The app's
   WebKit webview only talks `https` / `wss`.

## Running locally

The stack: LiveKit on `ws://localhost:7880` (dev keys `devkey` / `secret`), Postgres 16 and Redis in
Docker, the Go backend on `https://localhost:1926` (it also serves the web app), and the Tauri app,
which starts core as its sidecar. `backend/env-files/.env.local` already points the backend at all
of them.

1. **Postgres and Redis:** `task backend:compose-up` from the repo root starts them in Docker
   (`backend/dev-compose.yml`).
2. **LiveKit and backend:** `task dev-server` from the repo root. The backend task builds the web
   app, copies it into `backend/web/`, then runs the server, which creates its tables on start.
3. **Test users** (once, after the backend has started once):
   `cd backend && docker compose -f dev-compose.yml exec -T db psql -U hopp -d hopp < sql/mock_data.sql`.
   Gives `michael@dundermifflin.com` and `dwight@dundermifflin.com` in one team, password
   `hoppless`. `task backend:add-mock-data` does the same if you have `psql` locally.
4. **Desktop app:** `cd tauri && task dev`. It builds core to
   `core/target/debug/hopp_core-aarch64-apple-darwin` (the sidecar path Tauri expects), starts Vite on
   port 1420 and the app, which `tauri/.env.dev` points at `localhost:1926`.
5. **Sign in:** log in at https://localhost:1926, copy the "App token" from Settings, and paste it in
   the app under the `...` menu (bottom of the sidebar) > Debug > Auth Token. The app's Sign-in
   button ends in a `hopp://` link, which macOS hands to an installed app, not the dev build.
6. **A call needs two people.** For a second instance on the same Mac, run
   `cd tauri && task start-replica-app` after `task dev` has built once (Vite on 1421, its own state
   file) and sign it in as the other test user.

Working on the web app: `task dev-reload` from the root runs the backend and rebuilds and re-copies
the web app on every change (needs `fswatch`; it doesn't start LiveKit, so run
`task livekit:start-server` too if you need calls).

## macOS permissions

Hopp needs Screen Recording, Accessibility (to apply remote input), Camera and Microphone (System
Settings > Privacy & Security). macOS attributes them to the app that launched the process: for
`task dev`, `smoke.sh` or a bare `hopp_core` that is your terminal or editor, not Hopp. Screen
Recording only takes effect after you quit and reopen that terminal. The installed Dataico app has
its own grants, and an unsigned build loses them on every rebuild (`DATAICO.md`, "Build").

## Checks and tests

- **What to run** for each area, the same as CI: root `CLAUDE.md`, "Checks" (core: `core/CLAUDE.md`).
- **Smoke suite:** after changes to calls, IPC or screen sharing, run `core/tests/smoke.sh` (or
  `task core:smoke`). It makes real calls against a local LiveKit and prints `PASS` / `FAIL` per
  scenario. `core/tests/netem.sh` emulates bad networks (needs sudo), also under the real app.
  Setup and scenarios: `core/tests/README.md`.
- **The first core build is slow:** it downloads libwebrtc (several hundred MB). See
  `LK_CUSTOM_WEBRTC` in `DATAICO.md`, "Prerequisites".
- **Installing your build:** `task -d packaging/dataico install` from the repo root builds the
  current checkout and replaces `/Applications/hopp.app`. Signing, notarization, in-app updates,
  releasing (`task -d packaging/dataico release`) and logs: `DATAICO.md`.

## Where work lives

- **Status** lives only in `docs/TRACKER.md`. Update the row when you start, finish or drop work.
- **Specs:** a user-visible or cross-process feature gets `docs/specs/<ID>-<slug>.md`, copied from
  `docs/specs/TEMPLATE.md`, with its Behavior agreed before implementing. Bug fixes don't need one.
- **Divergence from upstream** gets a row in `DATAICO.md`, "What differs from upstream".
- **Read first:** `docs/ipc.md` before touching Tauri ↔ core IPC or Tauri state;
  `core/resources/effects/effects.md` before adding a screen effect; `DATAICO.md`, "Keeping in sync
  with upstream", before rebasing on `gethopp/hopp`.

## Gotchas

- **Wrong Node.** mise doesn't read `.nvmrc`, so until the repo's `mise.toml` is trusted your global
  Node (e.g. 24) runs, and the pinned Yarn breaks on it. `node -v` in the repo must print `v20.x`.
- **Backend on plain HTTP.** Without `backend/certs/` the backend only logs a warning and serves
  HTTP. The app and web app use `https` / `wss`, so nothing connects.
- **A web app type error stops the backend.** `task dev` in `backend/` runs the web app's
  `tsc -b && vite build` before starting the server.
- **Port 1420 must be free.** Vite exits instead of picking another port (a second Vite or a stale
  `task dev`).
- **Don't install the official Hopp app** next to ours: both register `hopp://` and are named
  `hopp.app`.
- More in root `CLAUDE.md`, "Gotchas".

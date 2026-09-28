# Agent Development Guide for Hopp

## Fork Policy (read first)

This is the `awkay/hopp` fork, maintained for our own users (Dataico build, see `DATAICO.md`). It is
not a staging area for upstream `gethopp/hopp` PRs.

- **Optimize for shipping to our users quickly.** Fix things the way that works best for us, even if
  it diverges from upstream's design. Don't shape a change around what upstream might accept.
- **Don't preserve upstream mergeability** at the cost of a better or faster fix. Refactors of
  upstream code (e.g. the Tauri `AppData` lock / core IPC) are fair game when they fix real problems.
- **No upstream negotiation.** We don't open upstream PRs or discuss design with upstream by
  default. Upstream is free to read and take our changes. Only prepare an upstream PR when explicitly asked.
- **Pulling from upstream is opt-in.** Merge upstream changes when they're useful to us; resolving
  conflicts in favor of our design is fine.
- **Record divergence.** When a change alters upstream behavior or architecture, add a line to the
  "What differs from upstream" section of `DATAICO.md` so the next person knows it's intentional.

## Project Overview

Hopp is an open-source pair programming app with screen sharing, remote control, and multi-user rooms. Built with Tauri (desktop), Go (backend), and Rust (core engine).

## Directory Structure

- `backend/` — Go API server (Echo, PostgreSQL, Redis)
- `core/` — Rust screen capture/remote control engine
- `tauri/` — Tauri desktop app (React + TypeScript frontend)
- `web-app/` — React web application
- `docs/` — Astro documentation site

## Tech Stack

- **Monorepo**: Yarn 4 workspaces, Node.js v20, Taskfile as primary entrypoint
- **Backend**: Go 1.25, Echo, PostgreSQL, Redis, GORM, JWT auth, Stripe, Sentry, LiveKit server SDK
- **Desktop**: Tauri 2 (Rust + React/TypeScript/Tailwind, Vite)
- **Core**: Rust — screen capture, remote input, LiveKit streaming, camera window and screensharing window (winit + iced + wgpu, native OS windows, not Tauri webviews)
- **Web App**: React + TypeScript, Vite, TanStack Query, Radix + Headless UI, Tailwind

## Commands

All commands use [Taskfile](https://taskfile.dev). Run `task --list` in any directory to see available tasks.

Do not run these as an agent — they require user interaction in a terminal/desktop.

**Backend:** `cd backend && task run` / `task test`
**Core:** `cd core && cargo build` / `cargo test` / `cargo fmt`
**Tauri:** `cd tauri && task dev` / `task build`
**Web App:** `cd web-app && yarn dev` / `yarn build`

## IPC Architecture

```
Tauri UI (React)  ←→  Tauri Backend (Rust)  ←→  Core Process (Rust)
   (webview)            (tauri commands)          (hopp_core sidecar)
```

- **UI → Tauri**: `invoke()` calls typed via `CommandMap` in `tauri/src/core_payloads.ts`
- **Tauri ↔ Core**: `socket_lib` — Unix socket (macOS/Linux) or TCP (Windows), length-prefixed JSON
  `Frame { request_id, message }`. All message variants in `core/socket_lib/src/lib.rs` (`enum Message`)
- **Core → UI**: core sends a `Message`; `CoreEventHandler::on_event` in `tauri/src-tauri/src/core_events.rs`
  emits `app.emit("core_<event>", payload)`, frontend listens via `listen("core_<event>", cb)`

### Protocol rules

- **Request ids.** Tauri tags requests with a `request_id`; core answers with a frame carrying the same id
  (`Application::reply` in `core/src/lib.rs`). Events (either direction) have no id. A response whose id
  nobody waits for (its request timed out) is discarded, so it can never be taken as the answer to a
  later request.
- **CoreClient** (`tauri/src-tauri/src/core_client.rs`, on top of `socket_lib::client::Client`) is the only
  way Tauri talks to core. `send` / `start_request` just enqueue on one ordered outbound queue (a writer
  thread does the I/O), so nothing can overtake anything else and callers never block. Requests are
  pipelined and awaited with `request(msg, timeout).await` (async commands) holding no lock. One
  dispatcher thread handles incoming frames in wire order: `on_response` sees a response before its
  request completes, `on_event` gets events.
- **Call ids.** The frontend assigns every call an id (`callTokens.callId`, `tauriUtils.callStarted`).
  `CallStart`, `CallStartResult`, `CallEnd(Option<id>)`, `CallEnded(id)` and
  `RoomConnectionFailed` carry it; each side ignores messages about a call that isn't its current one
  (`socket_lib::call`). A `CallEnd` for a call core no longer has does no teardown, only an
  acknowledging `CallEnded(id)`; `CallEnd(None)` (quit only) ends whatever is active. Hang-ups from
  a core window or a shortcut name the call they were made in. Tauri applies call side effects (shortcuts, dock icon, sleep prevention) when
  the matching message is processed (`tauri/src-tauri/src/call_state.rs`), never after an `await` in a
  command.
- **Core never asks Tauri and waits.** Anything core needs (e.g. the preferred camera) is pushed to it
  ahead of time (`SetPreferredCamera`, startup config in `Settings::core_startup_config`).
- **Tauri state** (`AppData`) has no global lock; see the rules on `AppData` in `tauri/src-tauri/src/lib.rs`:
  no lock across I/O or waits, main thread only touches atomics / main-thread-only state, global
  shortcuts are (un)registered only on the main thread, settings setters save and enqueue in one
  critical section.

### Adding a new IPC message

1. Add the variant to `enum Message` in `core/socket_lib/src/lib.rs`. If it concerns a call, include the
   `CallId`.
2. Handle it in core: map it to a `UserEvent` in `RenderEventLoop::run` (`core/src/lib.rs`), passing the
   frame's `request_id` if it is a request, and answer with `self.reply(request_id, response)`.
3. Tauri → core: fire-and-forget with `data.core.send(msg)`; request/response with
   `data.core.request(msg, REQUEST_TIMEOUT).await` in an `async fn` command, matching the expected
   response variant. If it is a persisted setting that core needs, save and send under
   `data.settings()` and add it to `Settings::core_startup_config` (so a restarted core gets it).
4. Core → UI: handle it in `CoreEventHandler::on_event` (`tauri/src-tauri/src/core_events.rs`) and
   `listen()` in the frontend.
5. Mirror new structs in `tauri/src/core_payloads.ts`.

## Code Style

- **JS/TS:** Prettier (120 cols). Pre-commit runs automatically.
- **Rust:** `cargo fmt` per crate (`core/`, `tauri/src-tauri/`).
- **Go:** `gofmt` + `golangci-lint` (config: `.golangcli.yml`; linters: `govet`, `ineffassign`, `unused`, `staticcheck`).
- **TS imports:** Use `@` alias → `src/` (configured in Vite for both `tauri/` and `web-app/`)
- **Frontend security:** Prefer normal React bindings; avoid `dangerouslySetInnerHTML`; use `new URL()` / `URLSearchParams` over string concatenation for URLs.

## Testing

- **Go:** Integration tests in `backend/test/integration/`
- **Rust:** Unit tests + visual integration tests in `core/tests/`
- **Frontend:** Linting + typechecking (no unit test runner configured)

## Key Conventions

- API contract: `backend/api-files/openapi.yaml` (OpenAPI); type-safe clients generated from it
- IPC contract: `socket_lib::Message` enum + TypeScript mirror in `tauri/src/core_payloads.ts`
- Cross-platform desktop: macOS/Windows/Linux — platform APIs and capture/overlay/input constraints matter
- Local dev uses mkcert HTTPS certs (WebKit requirement); Tauri dev expects Vite on port `1420`
- `hopp_core` binary bundled as external resource in the desktop bundle

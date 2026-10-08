# IPC and Tauri state

- **UI → Tauri**: `invoke()` calls typed via `CommandMap` in `tauri/src/core_payloads.ts`
- **Tauri ↔ Core**: `socket_lib` — Unix socket (macOS/Linux) or TCP (Windows), length-prefixed JSON
  `Frame { request_id, message }`. All message variants in `core/socket_lib/src/lib.rs` (`enum Message`)
- **Core → UI**: core sends a `Message`; `CoreEventHandler::on_event` in `tauri/src-tauri/src/core_events.rs`
  emits `app.emit("core_<event>", payload)`, frontend listens via `listen("core_<event>", cb)`

## Protocol rules

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

## Adding a new IPC message

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

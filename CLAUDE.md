# Hopp (Dataico fork)

## Invariants (read first)

§SPEC-WORKTREE: Work on each spec in its own dedicated Git worktree and feature branch, including
spec drafting and implementation. Never work on two specs in the same worktree or implement a spec
in the main checkout.

§SPEC-IMMUTABLE: Once a spec is done and merged, `docs/specs/<ID>-<slug>.md` and its
`notes.md` are a historical record: never edit them again, not even for typos, broken links or
outdated details. A later change to that behavior gets its own spec (or bug fix) that names what it
replaces, e.g. "replaces 0004 B3". Knowledge that must stay current lives in the topic docs
(`docs/`, `DATAICO.md`, `CLAUDE.md`), which are updated with the behavior. Until it is merged, a
spec is a working document and changes with the plan.

§TILING-WM: Hopp must behave correctly under AeroSpace and similar tiling window managers (yabai,
Amethyst), in every window style. Popups and overlays (the menu-bar popup, core's sharing overlay
and drawing window) stay unmanaged: any NSWindow style bit makes a window report
`AXStandardWindow`, and AeroSpace then adopts it into a workspace and jumps there whenever it is
shown (see the style-mask comment in `tauri/src-tauri/src/main.rs` setup). Standalone windows, like
the main window in the floating and regular styles, may be tiled or floated and must stay usable
at whatever frame the manager gives them. Showing, focusing or activating a window, or changing the
activation policy, must never switch the user's workspace or move a window to another one. Verify
every change to window creation, style masks, levels, focus, activation or positioning under
AeroSpace. When you can't run or emulate that check (agents never start the app), ask the user to
run it with concrete steps, and don't call the change done until they have.

§WORKTREE-CONTEXT: Every worktree must have a root-level [workingcontext.md](workingcontext.md)
with meaningful, non-empty content for the current session. This file is local and Git-ignored;
never commit it. Initialize it from [workingcontext.md.template](workingcontext.md.template) when
creating the worktree, filling in the actual session scope; if there are no discoveries yet, say so
explicitly. Read existing context before refreshing it for a new session, preserving relevant
branch discoveries until they are captured in the spec's committed notes. Record quirks, gotchas
and useful notes as they are discovered, and keep them accurate. This file holds session context
and discoveries, not plans or progress; status remains only in `docs/TRACKER.md`. Promote lasting
discoveries into the appropriate project docs before merging.

§READ-WORKING-CONTEXT: Always read this worktree's [workingcontext.md](workingcontext.md) in full
at the start of every session, before investigating or changing the project, even if `CLAUDE.md` was
provided automatically. If it is missing or empty, initialize it from `workingcontext.md.template`
and fill in meaningful session context before continuing; never leave it empty or with only a
heading or placeholder.

§SPEC-NOTES: Every successfully completed spec must preserve its discoveries in a committed
`docs/specs/<ID>-<slug>/notes.md`, linked at the end of `docs/specs/<ID>-<slug>.md`. Carry forward
all relevant knowledge from the worktree's `workingcontext.md`: notes, quirks, awkward behavior,
gotchas and bugs, distinguishing resolved bugs from remaining limitations. This is an essential
artifact of the work, not disposable scratch context; preserve the non-obvious details even when
lasting guidance is also promoted to other docs. If nothing was discovered, explicitly say so in a
non-empty file. Do not mark the spec complete, merge it or remove its worktree until the notes are
included in the committed deliverable.

§GPU-RESIDENT-VIDEO: On the normal macOS screen-sharing path, captured and decoded pixels remain
GPU-backed through hardware encoding, decoding and presentation. No full-frame CPU readback,
copying or format conversion in the normal path. Any necessary compatibility fallback must be
explicit, documented and observable, never silently become the default. CPU-based NV12 texture
uploads are a transitional optimization, not completion of this invariant.

§SCREEN-SHARE-FRAME-PACING: Coordinate capture, encoding and presentation around one frame-rate
target, with display-aware pacing and bounded queues that prefer the newest ready video frame over
a stale backlog. Normal mode targets sustained 60 delivered and presented frames per second during
motion, at the selected resolution, on healthy connections and capable displays. Lower rates must
reflect explicit low-bandwidth mode or actual source, network or hardware constraints, not an
arbitrary normal-mode cap. Verify actual delivered resolution, capture/encode/present cadence and
frame age; a configured encoder rate alone is not evidence of fluidity. Vsync is a measured
latency-versus-pacing choice, not a universally correct on/off setting.

§SCREEN-SHARE-TEXT-CLARITY: Normal mode on healthy connections and capable hardware must keep
text and fine UI edges crisp and readable at a reasonable viewing scale, including while typing,
editing or moving the cursor. Localized updates must not blur the whole view or require activity to
stop before text becomes sharp. Scrolling must be fluid, but reading text while it is actively
scrolling is not a requirement; after scrolling stops, text must promptly be crisp.

§CHANGE-DRIVEN-VIDEO: Encode, upload and redraw only for changed content, UI updates or active
animation deadlines. Static content must not cause recurring video work or idle redraws; block when
idle and schedule the next known deadline when animation is active. An update arriving while work
is in progress must remain pending until handled: never lose the final dirty state or its wakeup.
Protocol-required transport keepalives and codec recovery are separate from content updates and do
not justify ongoing black-frame encoding or timer-driven idle presentation.

## Fork policy

This is the `awkay/hopp` fork, maintained for our own users (Dataico build, see `DATAICO.md`). It is
not a staging area for upstream `gethopp/hopp` PRs.

- **Optimize for shipping to our users quickly.** Fix things the way that works best for us, even if
  it diverges from upstream's design. Don't shape a change around what upstream might accept.
- **Don't preserve upstream mergeability** at the cost of a better or faster fix. Refactors of
  upstream code (e.g. the Tauri `AppData` lock / core IPC) are fair game when they fix real problems.
- **No upstream negotiation.** We don't open upstream PRs or discuss design with upstream by
  default. Upstream is free to read and take our changes. Only prepare an upstream PR when explicitly asked.
- **Pulling from upstream is opt-in.** Merge upstream changes when they're useful to us; resolving
  conflicts in favor of our design is fine. If a merge brings back `AGENTS.md`, move anything useful
  into these `CLAUDE.md` files and delete it.
- **Record divergence.** When a change alters upstream behavior or architecture, add a line to the
  "What differs from upstream" section of `DATAICO.md` so the next person knows it's intentional.
- **Current fork-only work** (details in `DATAICO.md`): typing text while drawing, low-bandwidth mode,
  screen effects (see `core/CLAUDE.md`), the ordered request-id IPC (see `docs/ipc.md`), and the
  call-end CPU fix.
- **We only ship macOS on Apple Silicon.** CI and release builds cover `aarch64-apple-darwin` only;
  the Windows and Linux code is upstream's and nothing here compiles it.

## Docs, specs and tracker

`docs/` holds only what takes real investigation to rediscover: invariants, the non-obvious why,
flows that span files or processes, gotchas. Nothing that reading the code answers quickly, and no
indexes or file listings. Update a doc in the same commit as the behavior it describes.

- **New here?** Start with `docs/onboarding.md` (setup, permissions, running and testing locally).
- **Before touching Tauri ↔ core IPC or Tauri backend state** (`socket_lib`, `core_client.rs`,
  `core_events.rs`, `call_state.rs`, `AppData`): read `docs/ipc.md`.
- **Features:** a new user-visible feature or a change spanning processes gets a spec,
  `docs/specs/<ID>-<slug>.md` copied from `docs/specs/TEMPLATE.md`. Agree on its Behavior with the
  user before implementing. Bug fixes don't need one.
- **Status** of features and multi-session work lives only in `docs/TRACKER.md`. Update the row when
  you start, finish or drop work. Don't keep plans or progress anywhere else (no `tasks/todo.md`).

## Checks

Run the checks for what you changed and fix what they report; CI runs the same ones. Never start the
app, a dev server or an install (`task dev`, `yarn dev`, `task go`,
`task -d packaging/dataico install`): they need a desktop or never exit. Ask the user to run those.

- **core:** see `core/CLAUDE.md`.
- **Tauri backend** (`tauri/src-tauri/`): `cargo clippy --all-targets --all-features -- -D warnings`
  and `cargo test`. Both need `tauri/dist/` and the sidecar `core/target/debug/hopp_core-<host triple>`
  to exist; CI creates empty ones (`.github/workflows/tauri_rust_reusable.yml`).
- **Tauri UI** (`tauri/`): `yarn tsc --noEmit --skipLibCheck`.
- **Web app** (`web-app/`): `yarn tsc -b` and `yarn lint`.
- **Backend** (`backend/`): `go vet ./...`, `go test -tags=integration ./...` (in-memory SQLite and
  miniredis, no services needed) and `golangci-lint run --config ../.golangcli.yml`.
- **Formatting:** a hook in `.claude/settings.json` runs `rustfmt` on every `.rs` file you edit
  or write. Rust changed another way (a script, `sed`, a merge) still needs `cargo fmt` in its
  crate. `yarn prettier --write <files>` for TS/JS. CI rejects unformatted Rust, and the pre-commit
  hook that would format it only runs where someone ran `pre-commit install`.

## Code style

- **TS imports:** use the `@` alias for `src/` (both `tauri/` and `web-app/`).
- **Frontend security:** prefer normal React bindings; avoid `dangerouslySetInnerHTML`; use
  `new URL()` / `URLSearchParams` over string concatenation for URLs.

## Gotchas

- **API types are generated.** The contract is `backend/api-files/openapi.yaml`. After changing it,
  run `yarn generate-openapi-types` from the repo root, which rewrites `web-app/src/openapi.d.ts` and
  `tauri/src/openapi.d.ts`. Don't edit those two by hand.
- **Node 20** (`.nvmrc`). Yarn is pinned to 4.9.2 in `.yarn/releases/`, and newer Node breaks it.
- **Core's windows aren't webviews.** The camera and screen-share windows are native winit + iced +
  wgpu windows in the core process, not Tauri windows.
- Local dev uses mkcert HTTPS certs (a WebKit requirement).

# Core (`hopp_core`)

The screen-sharing and remote-control engine, run by the Tauri app as a sidecar. One user **shares**
their screen and others **control** it:

- **Sharer:** captures the screen and applies remote input.
- **Controller:** renders the remote frames and sends input.

Architecture and diagrams: `core/README.md`.

## Checks

- From `core/`: `cargo build`, `cargo test --workspace --lib` and
  `cargo clippy --all-features -- -D warnings`. Bare `cargo test` also picks up the harness in
  `core/tests/src/main.rs` and fails to compile.
- That clippy run, like CI's, skips unit tests. Lint them with
  `cargo clippy --lib --profile test --all-features -- -D warnings` (`--tests` also builds the
  harness and fails).
- The `svg_renderer` tests write badge PNGs into `core/`. Delete them; don't commit them.
- CI uses the latest stable Rust. Run `rustup update` if CI reports clippy lints yours doesn't.
- After changes to calls, IPC or screen sharing, run `core/tests/smoke.sh`: it makes real calls
  against a local LiveKit and prints `PASS` / `FAIL` per scenario (needs `brew install livekit`;
  macOS asks for Microphone access on the first run). Setup and scenarios: `core/tests/README.md`.
- Don't run the other harness tests in `core/tests/`: they need someone watching the screen.

## Conventions

- Platform code lives in `core/src/**/{linux,macos,windows}.rs`, selected with
  `#[cfg_attr(target_os = "macos", path = "macos.rs")] mod platform;`.
- A new module folder is declared as an inline `pub mod <folder> { pub mod <file>; }` block in
  `core/src/lib.rs`, like `graphics`. Don't add a `mod.rs`.
- Use descriptive names: `screen_sharing`, not `ss`.

## Screen effects (fork feature)

- Viewers send short animated stickers that play over the shared screen for everyone watching, one at
  a time per window.
- The assets are animated WebPs compiled into core; `core/build.rs` validates them against
  `core/resources/effects/effects.toml`.
- Before adding or changing an effect, read `core/resources/effects/effects.md` (manifest fields,
  caps, export recipes, runtime behaviour).
- Code: `core/src/effects*`, `core/src/graphics/effect_renderer.rs`, and the `effect` wire topic in
  `core/src/room_service.rs`. Effects need no IPC or Tauri changes.

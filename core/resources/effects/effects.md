# Screen effects

Screen effects are short animated stickers, such as a thumbs-up or confetti. A
viewer triggers one from the screen-share window, and it plays for a few seconds
over the shared screen for everyone watching.

This document describes:

- the manifest format;
- the limits on effects, and why each limit exists;
- how to author and add an effect;
- what the build checks;
- how effects behave at run time.

Everything lives in this directory:

| File | Purpose |
|---|---|
| `effects.toml` | The manifest. It lists every effect. |
| `*.webp` | The assets, as animated WebP files with transparency. |
| `*_icon.png` | Optional hand-made picker icons (`icon`). |
| `tools/make_placeholders.py` | Regenerates the placeholder assets and `confetti_icon.png`. |
| `tools/make_test_fixtures.py` | Regenerates the tiny fixtures that the validator's tests use (`core/src/effects/testdata/`). |

Effects are **compiled into `hopp_core`**. `core/build.rs` reads the manifest,
decodes every frame of every asset, and fails the build on any rule violation.
At run time nothing is loaded from disk, so a bad asset cannot ship. Adding or
changing an effect therefore needs a rebuild.

## Manifest (`effects.toml`)

```toml
schema = 1

[[effect]]
id = "thumbs_up"          # required
label = "Thumbs up"       # required
file = "thumbs_up.webp"   # required
loops = 4                 # optional, default 4
height_fraction = 0.25    # optional, default 0.33
order = 10                # optional, default 0
thumbnail_frame = 7       # optional, default: the most opaque frame
icon = "thumbs_icon.png"  # optional, default: none (thumbnail from a frame)
```

Unknown fields, at the top level or inside `[[effect]]`, fail the build. A typo
like `heigth_fraction` is reported instead of being silently ignored. A field
with the wrong type also fails the build, for example `loops = "infinite"`.

### Top level

| Field | Type | Allowed values | Default | Meaning |
|---|---|---|---|---|
| `schema` | integer | `1` | required | The manifest format version. |
| `effect` | array of tables | 0 to 24 entries | empty | One `[[effect]]` table per effect. |

### `[[effect]]`

| Field | Type | Allowed values | Default | Meaning |
|---|---|---|---|---|
| `id` | string | `^[a-z0-9_]{1,32}$`, unique | required | The identifier sent over the network. **Append-only: never rename or reuse an id.** Clients running an older or newer build look effects up by id, and they silently drop ids they don't have. |
| `label` | string | 1 to 24 characters, not blank | required | The tooltip in the picker. It stays on this machine and is never sent. |
| `file` | string | a bare file name in this directory ending in `.webp`; no `/`, `\`, `..` or leading `.` | required | The asset. It must exist. |
| `loops` | integer | 1 to 16 | `4` | How many times the frames play. The WebP file's own loop count is ignored. There is no "infinite". Every bundled effect plays at least 4 times; give very short clips more loops so they stay on screen for about 3 s. |
| `height_fraction` | float | 0.05 to 0.66 | `0.33` | The height of the effect on screen, as a fraction of the height of the shared screen (or of the shared window). The width follows from the asset's aspect ratio. |
| `order` | integer | any `i32` | `0` | The position in the picker, sorted ascending. Effects with the same value keep their manifest order. |
| `thumbnail_frame` | integer | 0 to (frame count - 1) | the frame with the most opaque pixels | The frame the picker thumbnail is made from (see [Picker thumbnail](#picker-thumbnail)). Ignored when `icon` is set. |
| `icon` | string | a bare file name in this directory ending in `.png`; no `/`, `\`, `..` or leading `.` | none | A hand-made picker icon used instead of a thumbnail generated from a frame. It must be an 8-bit RGBA PNG, 32 to 256 px on each edge, roughly square (longer edge at most 1.25 × the shorter) and at most 64 KB. |

### Picker thumbnail

Each effect gets a 64 × 64 RGBA picker thumbnail, made at build time:

- **With `icon`:** the PNG is scaled whole to fit the square (aspect ratio kept, centred). Your framing and padding are kept, so leave only a small margin.
- **Without `icon`:** the build takes `thumbnail_frame` (default: the frame with the most opaque pixels), **crops it to the bounding box of its pixels with alpha above 16**, adds padding of 6% of the crop's longer edge on each side (clamped to the canvas), and scales that to fit the square (aspect ratio kept, centred). A small subject on a large, mostly transparent canvas therefore still fills the icon.
- **Coverage check:** if fewer than **5%** of the final thumbnail's pixels have alpha above 128, the build fails. The icon would look empty in the picker. This happens with effects made of many small scattered pieces (such as confetti), where no crop helps. Set `icon` to a drawn PNG, or `thumbnail_frame` to a fuller frame. For example:

  ```
  - effect confetti: thumbnail: the generated picker thumbnail is nearly invisible: 0.0% of its pixels have alpha > 128, at least 5% are needed. Set `icon = "<name>.png"` (a hand-made picker icon) or `thumbnail_frame` (a fuller frame) for this effect
  ```

## Caps and why they exist

The build enforces all of these limits. They are defined as constants in
`core/src/effects/manifest.rs`.

| Rule | Cap | Why |
|---|---|---|
| Canvas | at most **1280 × 720** | This allows an effect of about 66% of a 1080p screen at native resolution. One decoded frame is then at most 3.7 MB, which bounds memory during playback (see below). |
| Canvas | at least **32 px** on each edge | Anything smaller is invisible at the smallest `height_fraction`. It is almost always an export mistake. |
| Frames | at most **72** stored frames | This bounds decode work. It is plenty for about 3 s at 24 fps. Hold frames (see below) instead of duplicating them. |
| Per-frame delay | **20 to 1000 ms** | Delays of 0 to 10 ms mean different things in different decoders; browsers clamp them to about 100 ms. Anything under 20 ms is faster than a redraw, so those frames would never be seen. The build rejects such delays rather than clamping them. Longer holds are fine up to 1 s. Chain frames if you need more. |
| One loop | at most **3000 ms** | Effects are reactions, not videos. |
| `loops` | **1 to 16** | There is no infinite loop. An effect must end without anyone acting. 16 lets a 200 ms clip stay on screen for about 3 s. |
| Total play time | `loops` × loop duration at most **12000 ms** | One effect blocks all others while it plays, so a long one blocks everyone else's reactions. 12 s fits a 3 s loop played 4 times. |
| File size | at most **3 MB** per file | These bytes are compiled into the binary and stay resident. |
| All files | at most **24 MB** total | This keeps the binary and resident size bounded. |
| Effects | at most **24** | The picker must stay a small grid. |
| Format | an **animated** WebP **with an alpha channel** | Effects are drawn over arbitrary screen content, so they need real transparency. A still WebP is rejected. |

## Adding an effect

1. **Author the animation** on a transparent background:
   - The canvas must be at most 1280 × 720. A convenient size for a large effect is 960 × 540.
   - Leave the background fully transparent. Anti-aliased edges should be partially transparent, not blended against a colour.
   - Make it at least 1 frame and no more than 72 frames long.
   - Plan the timing per frame. For example, scale in over 7 quick frames (30 to 40 ms each), hold one frame for 800 ms, then fade over 5 frames (50 ms each).
   - Author the fade-out into the asset. The player does not fade the effect; it stops drawing the moment the last frame's time is up.
2. **Export an animated WebP** with alpha. See the next section.
3. **Copy the file** into `core/resources/effects/`. Use a lower-case file name with no spaces.
4. **Add an `[[effect]]` table** to `effects.toml`:
   - Pick a new `id`, and never reuse an old one.
   - Set `label` and `file`.
   - Set `height_fraction` to the intended size. Use about 0.2 to 0.3 for a sticker and up to 0.66 for a full-screen celebration.
   - Set `loops`.
   - Set `order` to place it in the picker.
   - If the build reports that the picker thumbnail is nearly invisible, set `icon` (a PNG in this directory) or `thumbnail_frame`.
5. **Build core** (`cd core && cargo build`, or `task build_dev`). Any violation fails the build, and the message names the effect, the field and the rule.
6. **Run the tests**: `cargo test --lib effects`. One test decodes every bundled asset again and checks it against the metadata recorded at build time.
7. **Try it in a call.** From the screen-share window, open the wand menu and pick the effect. Check the size and timing on both a Retina display and a 1× display.

## Exporting animated WebP with transparency

WebP stores a duration for each frame, so "hold this pose" is one frame with a
long delay, not 20 copies of the same frame. Aim for lossy colour (quality 75 to
85) with lossless alpha: that is the smallest output that still gives clean
edges.

### libwebp: `img2webp` (from a PNG sequence)

This is the recommended tool, and the one `tools/make_placeholders.py` uses. It
is available as `brew install webp` or `apt install webp`. Each `-d` sets the
duration in ms of the frames that follow it:

```sh
img2webp -loop 1 -lossy -q 80 -m 6 \
  -d 30 in_000.png -d 30 in_001.png -d 30 in_002.png \
  -d 800 in_003.png \
  -d 50 in_004.png -d 50 in_005.png \
  -o my_effect.webp
```

- `-lossy -q 80` compresses the colour lossily. `img2webp` keeps the alpha plane lossless by default.
- If two consecutive frames are identical, `img2webp` merges them into one frame with the summed duration. The result must still be at most 1000 ms, so chain distinct frames for longer holds.
- `-loop` does not matter here, because the manifest's `loops` is used instead.
- Inspect the result with `webpmux -info my_effect.webp`. It lists every frame's duration and should show `Features present: animation transparency`.

### libwebp: `gif2webp` (from an existing GIF)

```sh
gif2webp -lossy -q 80 -m 6 in.gif -o my_effect.webp
```

This is acceptable for a quick conversion. However, GIF transparency is 1-bit,
so the edges stay jagged: see "Why not GIF" below. Re-export from the source
with real alpha when you can.

### ffmpeg (from a PNG sequence or a video with alpha)

ffmpeg's `libwebp_anim` encoder keeps alpha when the input has it. Example
inputs with alpha are a PNG sequence, ProRes 4444 or VP9 with alpha.

```sh
# Constant frame rate from a PNG sequence:
ffmpeg -framerate 25 -i in_%03d.png -c:v libwebp_anim -lossless 0 -q:v 80 \
  -pix_fmt yuva420p -loop 1 my_effect.webp
```

ffmpeg writes the same delay for every frame, and has no simple per-frame hold.
To hold frames, export PNGs with ffmpeg (`ffmpeg -i in.mov out_%03d.png`) and
pack them with `img2webp`, which takes a `-d` for each frame. Always check
`webpmux -info` for `transparency`. If it is missing, the input had no alpha
channel, and the build will reject the file.

### Aseprite

Use **File → Export → Export As…** and choose `.webp`, or export a PNG sequence
and use `img2webp`.

- Aseprite keeps each frame's duration from the timeline (Frame Properties).
- Use a transparent background layer, not a filled one.
- Newer Aseprite versions have WebP options. Choose lossy with quality 80. Lossless is fine too, but the file is larger.

### After Effects (and other Lottie sources)

Render the composition with alpha, or export it to Lottie (Bodymovin), then
convert it to animated WebP.

- **From After Effects directly:** render to a PNG sequence with the RGB + Alpha channels. Set straight or premultiplied alpha in the output module; both work, because the build requires only that alpha is present. Then pack the frames with `img2webp`, setting `-d` per frame for holds.
- **From Lottie JSON:** use a Lottie → WebP converter. Examples are `lottie-to-webp`/`puppeteer-lottie` scripts, `rlottie`'s `lottie2gif`-style tools, LottieFiles' export, and `dotlottie` CLI converters. Export at the final pixel size, check that the background is transparent, then check the result with `webpmux -info`.
- Keep the composition at or under 1280 × 720 and 3 s, and bake in the ease-out and fade.

### Why not GIF, and why not Lottie

- **GIF:** GIF transparency is 1-bit: a pixel is either fully opaque or fully transparent. Anti-aliased edges therefore get a halo of whatever colour they were matted against. Over arbitrary screen content (dark IDEs, white documents) that looks bad. GIF is also limited to 256 colours and gives larger files than lossy WebP.
- **Lottie:** Lottie is vector, tiny and resolution-independent. However, playing it in core would need a vector renderer. The Rust options pull in a different wgpu version (velato/vello) or C++ libraries (thorvg, rlottie), and each covers Lottie features only partly. They would add a per-frame CPU or GPU raster cost and cross-platform build risk to a process that must never stall. Pre-rendering to WebP moves all of that to authoring time.
- **APNG** also has full alpha, but its files are 2 to 5 times larger than lossy WebP, and fewer tools export it.

## What the build validates

`core/build.rs` includes the same validator as the crate
(`core/src/effects/manifest.rs`) through `#[path]`. The validator has unit
tests for every rule. For `effects.toml` and each entry it checks:

1. **Parsing.** The TOML must be valid, with no unknown fields and the correct types.
2. **The manifest.** `schema` must be 1, and there must be at most 24 effects.
3. **Each entry's fields.** The `id` charset, length and uniqueness; the `label` length; `loops` from 1 to 16; `height_fraction` from 0.05 to 0.66; and a bare `.webp` `file` name.
4. **The file itself.**
   - It must exist and be at most 3 MB.
   - It must have the RIFF/WEBP magic bytes.
   - It must be animated and have an alpha channel.
   - The canvas must be at most 1280 × 720 and at least 32 px on each edge.
5. **Every frame, decoded.**
   - There must be at most 72 frames.
   - Each delay must be from 20 to 1000 ms.
   - One loop must last at most 3000 ms.
   - `loops` × loop must be at most 12000 ms.
   - `thumbnail_frame` must be within range.
6. **The picker thumbnail.** An `icon` must exist and pass the PNG rules above. The final thumbnail (from `icon`, or the cropped frame) must pass the 5% coverage check.
7. **All files together.** They must add up to at most 24 MB.

The build reports every error at once, not only the first. For example:

```
Invalid screen effects in .../core/resources/effects/effects.toml (3 error(s)):
  - effect star_bounce: height_fraction: 0.8 is out of range; must be 0.05..=0.66
  - effect #2: id: "Confetti!" must match ^[a-z0-9_]{1,32}$
  - effect #2: loops: 17 is out of range; must be 1..=16
See core/resources/effects/effects.md for the rules.
```

- An effect whose `id` is itself invalid is shown by its position, for example `#2`.
- Parse errors show the TOML location:

```
Invalid screen effects in .../effects.toml (1 error(s)):
  - effects.toml: TOML parse error at line 6, column 1
  |
6 | size = 128
  | ^^^^
unknown field `size`, expected one of `id`, `label`, `file`, `loops`, `height_fraction`, `order`, `thumbnail_frame`, `icon`
```

Asset errors look like these:

- `effect wave: canvas: "wave.webp": 1290x40 exceeds the 1280x720 maximum`
- `effect wave: frame delay: frame 1 lasts 10 ms; must be 20..=1000 ms`
- `effect wave: file: "wave.webp": has no alpha channel; effects must be transparent`
- `effect wave: loops: 9 loops of 1500 ms play 13500 ms; must be at most 12000 ms`

On success, the build generates `$OUT_DIR/effects_gen.rs`. That file holds the
`EFFECTS` table (id, label, loops, height_fraction, canvas size, the per-frame
delays, and the compressed bytes via `include_bytes!`), plus a 64 × 64 RGBA
thumbnail for each effect.

## Runtime behaviour

**Who can trigger an effect.** A viewer can, from the screen-share window, using
the wand button next to the settings cog. It opens a grid of thumbnails, and
hovering over one shows its label. Clicking a thumbnail plays the effect in
your own window straight away and sends it to the room. The sharer and
camera-only participants cannot trigger effects in this version.

**Who sees an effect, and where.**

- **The sharer** sees it on the overlay over the shared screen. The overlay is not captured, so the effect never appears in the video itself.
- **Every other viewer** sees it in their screen-share window, if that window is visible.
- **Camera-only participants and web viewers** do not see effects.
- Effects only show during a call. A trigger that arrives after the call ended is dropped.

**One at a time.** Each window plays at most one effect. If a trigger arrives
while an effect is playing, from anyone including yourself, it is **dropped
silently**, not queued. While an effect plays, the wand button and the picker
are greyed out and do nothing. Two nearly simultaneous triggers can show
different effects on different machines: each machine plays whichever reached
it first. This is accepted.

**Placement and size.**

- The effect is centred on the shared content: the shared screen, or the shared window when a single window is shared.
- Its height is `height_fraction` of that content's height, and the aspect ratio is kept. If that would make it wider than the content, it is shrunk to fit.
- The packet has an `at` field for a position, which is kept for later placement. This version always sends `null` and ignores it on receipt.

**Timing.**

- Each frame is shown for its own delay. The frame is chosen from the elapsed time against the cumulative delays, never from a fixed frame rate.
- If decoding falls behind, frames are skipped rather than slowing the animation.
- The effect ends after `loops` passes. When it ends, it disappears and the picker is enabled again.

**Resources.**

- While idle, an effect costs only its compressed bytes, which are compiled into the binary.
- On play, a worker thread decodes frames one by one and runs one to two frames ahead of what is shown.
- The window uploads a frame into its own GPU texture only when the shown frame changes. It draws the texture as an alpha-blended quad after the rest of the window.
- At the 1280 × 720 cap this is about 11 MB of frames in memory, plus one texture.
- The worker, the frames and the texture are all freed when the effect ends, when the window hides, when the viewer window switches to a new sharer, or when the call ends.

**GPU failures.**

- Every effect wgpu call (pipeline, texture, uploads, the draw pass) runs inside Validation and OutOfMemory error scopes and `catch_unwind`, so a GPU error cannot reach wgpu's default handler (which panics) and kill core.
- On an error the window logs it at error level, stops the effect, drops its GPU objects and disables effects for the rest of the call (the wand button greys out, incoming effects are ignored in that window). The next call re-enables them.

**Network.**

- The packet is `{"v":1,"id":"<id>","at":null}` on the LiveKit data topic `effect`.
- It is sent **lossy**. A lost effect is harmless, and the packet must never queue up in front of keystrokes on the reliable channel.
- Receivers drop the packet silently if:
  - it is larger than 256 bytes;
  - it is not valid JSON;
  - `v` is not 1;
  - its id is malformed or unknown to this build;
  - it has no sender;
  - it is their own echo.
- A point that is out of range is clamped.
- Clients without this feature log one error line per effect packet and otherwise ignore it.

**Code map.**

| Path | What it does |
|---|---|
| `core/src/effects/manifest.rs` | The caps and the validator, shared with `build.rs`. |
| `core/src/effects.rs` | `EffectDef` and the generated table. |
| `core/src/effects/wire.rs` | The packet and its validation. |
| `core/src/effects/player.rs` | One-at-a-time playback and the decode worker. |
| `core/src/graphics/effect_renderer.rs` | The GPU texture and quad. |
| `core/src/shaders/effect_quad.wgsl` | The shader for the quad. |
| `core/src/window/screensharing_window.rs` | The picker, and the viewer's side. |
| `core/src/graphics/graphics_context.rs` | The sharer overlay's side. |
| `core/src/room_service.rs` | Topic `effect`: publishing and receiving. |

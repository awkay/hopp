# Unsafe Rust

We don't write `unsafe` Rust (`CLAUDE.md` §NO-UNSAFE). This file keeps the places where `unsafe`,
or the dependency change that would make it unnecessary, would bring a real benefit, so they can be
picked up later. Work on one gets a row in `TRACKER.md`, where its status lives.

Each entry says where, why the safe version is what it is, what that costs, the `unsafe` version
if one was written, and how to get the benefit without `unsafe`.

## Audio output callback: one frame allocation per participant per mix

- **Where:** `core/src/audio/mixer.rs`, `MixerSource::get_audio_frame_with_info`, on CoreAudio's
  real-time output thread.
- **Why it allocates:** our LiveKit fork's `AudioMixerSource` trait
  (`libwebrtc/src/native/audio_mixer.rs`) asks for each frame through `&self` and returns one that
  borrows from `self`: `get_audio_frame_with_info(&self, target_sample_rate) ->
  Option<AudioFrame<'_>>`. Reading a frame advances the participant's queue, so lending a reused
  buffer means mutating through `&self` and returning a borrow of the result. A lock guard can't
  do that, because it ends with the call, before the frame is used. So each frame is a new `Vec`
  (`Cow::Owned`), which the fork's wrapper frees after copying it into libwebrtc's frame.
- **What it costs:** one 320-byte allocation and one free per remote participant every 10 ms, both
  on the real-time thread. `rendering_allocates_only_the_frames_lent_to_the_mixer` pins that count
  and checks nothing else on the render path touches the heap (it counts Rust allocations only,
  not libwebrtc's C++ ones). Small same-thread allocations are fast on macOS, but malloc can take
  a lock, so under load or memory pressure it's a glitch risk. Not measured.
- **The `unsafe` version (written and rejected on 2026-10-09):** `queue: UnsafeCell<SourceQueue>`,
  `let queue = unsafe { &mut *self.queue.get() };` in `get_audio_frame_with_info`, returning
  `Cow::Borrowed` of a frame buffer the queue reuses. It is sound only because
  `AudioMixer::mix(&mut self)` is the sole caller, serialized by core's mixer lock, and the
  wrapper copies the frame (`update_frame`) before returning, so no borrow outlives the call. None
  of that is checked by the compiler.
- **How to get it safely:** change the fork's trait so the source writes into a buffer instead of
  lending one, e.g. `fn fill_audio_frame(&mut self, target_sample_rate: u32, out: &mut [i16]) ->
  bool`. The wrapper hands the source to C++ inside an `Arc`, so it would keep it in a `Mutex` and
  copy into the native frame while holding it; no borrow escapes. Core would then pop each frame
  from the participant's ring straight into the buffer, and the callback would stop touching the
  heap. Needs push access to `awkay/rust-sdks`.

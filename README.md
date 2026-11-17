# reaction-player

A frame-accurate multi-stream video player for reaction videos and alignment
work: up to four videos stacked on one canvas, each with its own offset down to
the millisecond, plus a union timeline that starts the moment any layer starts.

Written in Rust with [egui](https://github.com/emilk/egui) / eframe for the UI,
[wgpu](https://wgpu.rs) for compositing, and FFmpeg for decode.

> **Status: M1 — source complete, not yet compiled.**
> All 43 source files exist and are statically validated (parse, module paths,
> method resolution, WGSL). No Rust toolchain was available in the environment
> where this was written, so `cargo build` has never run. See
> [Verification](#verification) for exactly what has and has not been checked.
> Do not treat this as a working binary until it builds.

## What it does

- **Multi-stream canvas.** One session holds up to 4 layers. Each layer is a
  normalized rectangle on the canvas, so a layout survives any window size.
- **Per-layer offsets.** `offset_us` per layer, editable by typing, by `,`/`.`
  nudges, or by dragging. The union timeline starts when the earliest layer
  starts; nothing is clipped.
- **Frame-accurate transport.** Space to play/pause, arrows to seek while
  playing and to step exactly one frame while paused. Seeks are
  `avformat_seek_file` → `avcodec_flush_buffers` → decode-and-discard until the
  target pts, so a seek lands on the frame you asked for.
- **Alignment Studio.** Capture a frame from each side, then compare them with
  **Onion** (cross-fade) or **Wipe** (split with an accent divider). The studio
  reports `pts_B − pts_A` and offers a one-click "Apply to B" that writes the
  offset and rebuilds the timeline.
- **Hardware decode with fallback.** VAAPI → Vulkan → VDPAU on Linux, D3D11VA →
  DXVA2 on Windows, VideoToolbox on macOS. Any failure falls back to software
  decoding and says so in the layer badge, rather than failing the import.
- **Sessions and presets.** Sessions are JSON with a `version` field; four
  built-in layout presets (split, 2×2 grid, and two picture-in-picture
  arrangements) plus user presets.

## Requirements

| What | Version | Why exact |
| --- | --- | --- |
| Rust | 1.95+ | eframe 0.37 sets `rust-version = 1.95`; the code uses let-chains and edition 2024 |
| FFmpeg | **9.x** (libavcodec ≥ 63.1) | `ffmpeg-next`'s major tracks FFmpeg's. 9.x ↔ FFmpeg 9.x, 8.x ↔ FFmpeg 8.x. A mismatch is an ABI mismatch, not a warning |
| clang / libclang | any recent | `ffmpeg-sys-next` runs bindgen at build time |
| pkg-config, a C toolchain | — | to find and link FFmpeg |
| X11 or Wayland dev headers, Vulkan loader | — | eframe's windowing and wgpu's surface |
| ALSA (Linux) | — | cpal output |

Shortcut for the common distros:

```sh
./scripts/install-deps.sh           # installs what is missing (Arch/Debian/Fedora)
./scripts/install-deps.sh --check    # reports only; exits 1 if anything is missing
```

Three helper scripts ship with the source:

| Script | What it does |
| --- | --- |
| `scripts/install-deps.sh` | installs/verifies build dependencies, and checks the FFmpeg major version pairing |
| `scripts/calibrate.sh` | computes `pts_B − pts_A` from two files and timestamps, using `ffprobe` |
| `scripts/make-demo-clips.sh` | writes the demo clips by hand; the app normally generates them itself |

### FFmpeg pairing

This is the one version constraint that bites. `Cargo.toml` pins
`ffmpeg-next = "9.1"`, which requires FFmpeg 9.x headers and shared libraries.
Check what you have:

```sh
pkg-config --modversion libavcodec   # want 63.x or later, i.e. FFmpeg 9.x
```

If your distro ships FFmpeg 7 or 8, either install FFmpeg 9 alongside it or
build FFmpeg 9 from source. Do *not* "fix" the build by loosening the crate
version: `ffmpeg-next` 8.x has a different `Frame::wrap` ownership model and
different side-data types than the 9.x code here. The FFmpeg 9 ABI is what
`src/engine/track.rs` and `src/engine/hwaccel.rs` were written against.

## Build

```sh
cargo build --release
cargo test              # unit tests: geometry, timeline, drift, store, shader-adjacent maths
./target/release/reaction-player
```

On Arch, the included `PKGBUILD` does the same with `makepkg -si`.

## Use

1. **New session** → pick a canvas aspect (16:9, 9:16, 4:3, 1:1, 21:9, or fill
   the window) and a layout preset.
2. **Add layer** → choose a file. The layer appears where the preset put it;
   drag to move, drag a corner handle to resize, `Shift` to keep the aspect.
3. **Set offsets** → use the offset steppers in the layer panel, `,`/`.` for
   1 ms nudges, or the Alignment Studio for a measured value.
4. **Align** → `C` captures a frame on each side, `P` cycles Onion/Wipe, then
   **Apply to B** writes the offset.
5. **Save** — sessions are written on every edit that changes them (and on
   open/close), to a JSON file under the data directory. There is no separate
   "save as" yet; duplicating a session means copying its JSON file and giving
   it a new `id`.

### Keyboard

| Keys | Action |
| --- | --- |
| `Space` | Play / pause |
| `Left` / `Right` | Seek while playing; step one frame while paused (`Shift` = 10×) |
| `,` / `.` | Nudge the selected offset by 1 ms |
| `[` / `]` | Move the selected layer down / up one z step |
| `Shift`+`[` / `Shift`+`]` | Send to back / bring to front |
| `C` | Capture a frame for alignment |
| `P` | Cycle Onion / Wipe |
| `M` | Mute / unmute master audio |
| `L` | Link transports |
| `T` | Theater mode |
| `F` | Fullscreen |
| `?` | Cheat sheet |
| `Esc` | Close the topmost window |

### Where files live

`directories::ProjectDirs::from("dev", "reaction", "reaction-player")`:

- Linux: `~/.config/reaction-player/` (preferences, presets) and
  `~/.local/share/reaction-player/` (sessions, thumbnails, demo media)
- macOS: `~/Library/Application Support/dev.reaction.reaction-player/`
- Windows: `%APPDATA%\reaction\reaction-player\data\`

If the store cannot be created the app still starts, using a temp directory,
and shows a red banner saying files will not be saved. That banner is not
decoration: it means the data directory is unwritable.

## Calibration, from the command line

`scripts/calibrate.sh` runs the same arithmetic the Alignment Studio runs, so a
disagreement can be reproduced outside the GUI:

```sh
./scripts/calibrate.sh --a source.mp4 --b reaction.mp4 \
    --a-frame 00:00:12.480 --b-frame 00:00:14.100
```

It prints `pts_A`, `pts_B` and `delta = pts_B − pts_A` (µs) and warns when the
delta is large enough to suggest the two timestamps are not really the pair of
frames you meant. It needs `ffprobe`, not the app.

## How it works

```
src/
  main.rs            window, FFmpeg init, eframe entry
  app.rs             App state, screen routing, per-frame driver, key handling
  error.rs           AppError (one error type, string payloads)
  model/             pure data, no I/O and no GPU
    geom.rs          NormalizedRect, PixelRect, Handle::resize, snap_value
    layout.rs        canvas aspects, fit modes, normalized→pixel
    session.rs       Session / LayerConfig, schema version 1
    timeline.rs      union bounds, per-layer media time
    presets.rs       four built-ins, user presets
    settings.rs      Preferences
    store.rs         JSON persistence, atomic writes, id generation
  engine/            decode, timing, audio; one thread per layer
    track.rs         demux + decode worker, frame-accurate seek, history
    hwaccel.rs       device probing, get_format, hw→system transfer
    sync_clock.rs    master clock, drift controller with hysteresis
    audio.rs         cpal output callback, SPSC rings, mixing
    audio_resample.rs fractional drift resampler
    runtime.rs       per-frame tick: which frame each layer should show
    video_frame.rs   pooled YUV planes, stride-preserving copies
    demo.rs          generated demo media so an empty install shows something
  gpu/               wgpu compositor
    shader.wgsl      one quad per layer, YUV→RGB in the fragment shader
    renderer.rs      textures, uniforms, bind groups, pipeline
    compositor.rs    PaintList, egui paint callbacks, cache retirement
    capture.rs       Alignment Studio frame capture ring
  ui/                screens are plain functions over &mut App
    theme.rs, widgets.rs, shortcuts.rs, toast.rs
    screens/         top_bar, transport, layers, canvas, alignment, modals, …
```

Three design decisions worth knowing about:

**One thread per layer, UI never blocks.** A `TrackHandle` exposes only atomics
and channels. The UI polls it; the decode thread never waits on the UI.

**Planes keep FFmpeg's strides.** Decoded planes are copied once into pooled
buffers, and `linesize` is handed to `Queue::write_texture` as
`bytes_per_row`. Rows are never repacked, so a 1920-wide frame with a 1920-byte
stride costs one copy, not two. Padding is uploaded but never sampled.

**The clock is audio-driven when audio exists.** The master clock advances by
the frames the cpal callback consumed, with the device-vs-callback timestamp
delta as latency compensation. Each layer gets a drift controller with
hysteresis (ignore below 5 ms, nudge below 100 ms, seek above it), so video
follows audio instead of fighting it.

## Verification

Honest accounting, because this matters more than a green checkmark:

**Checked.** Every `.rs` file parses with tree-sitter-rust; every `crate::` path
resolves to a real item; every `mod`/file pair lines up; all 508 distinct method
names used either resolve to an in-crate definition or appear in a std/external
API allowlist; `shader.wgsl` parses with `wgsl_reflect` and its `vs_main` /
`fs_main` entry points and uniform layouts were checked against the Rust
`#[repr(C)]` structs. External APIs were verified against cloned upstream
sources at the pinned versions (egui 0.37.0, rust-ffmpeg 9.1.0,
ffmpeg-sys-next 9.1.1, cpal 0.19.0, ringbuf 0.6.0).

**Not checked.** Nothing has been compiled, linked, or run. Type inference,
trait-object coercions, lifetimes, borrow conflicts and wgpu validation are all
unproven. The unit tests have not executed. Expect to fix compile errors on the
first real `cargo build` — the verification above catches structural mistakes,
not all of them.

**Known gaps.**

- Rotation: `rotate_frame` transposes the planes for 90/270 and flips for 180,
  but only for 8-bit 4:2:0 (the two formats the pipeline accepts). Hardware
  frames are transferred to system memory before rotation, as they must be.
  The angle comes from the stream's display-matrix side data via the same
  negated-`atan2` arithmetic as `av_display_rotation_get`.
- GPU cross-file type assumptions (`FrameUpload`, `CompareUpload`, `LayerDraw`)
  were unified by hand and not compiler-checked.
- wgpu 30 descriptor names were taken from the vendored sources; if a call site
  still uses a pre-30 name it will fail at build time, loudly.
- The GPU upload path (`layer_entry` / `compare_entry` / `upload_frame`) is
  written against FFmpeg's `linesize` as `bytes_per_row` but has never run, so
  a stride/shape mistake would show up as a distorted image, not a build error.
- The audio callback's latency compensation assumes the device reports a
  playback timestamp at or after the callback timestamp; a device that reports
  neither yields 0 µs instead of panicking.

## License

MIT OR Apache-2.0.

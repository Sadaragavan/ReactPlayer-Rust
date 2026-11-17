# Reaction Player

A frame-accurate multi-stream video reaction player and alignment tool, written
in Rust with `egui`/`wgpu`, decoding with FFmpeg.

Open two to four video files, lay them out on one canvas, crop and stack them,
and play them **in sync to the frame**: a source video with one or more reaction
recordings next to it, a picture-in-picture reaction over the source, a grid, or
any arrangement you drag together. When a reaction was recorded separately, the
Alignment Studio measures the offset between the two recordings and writes it
into the session.

* **Frame-accurate sync.** One master clock; every layer's target is
  `master + offset`. Drift under 5 ms is ignored, 5–100 ms is absorbed (frames
  held or dropped for picture, a ±0.5 % rate nudge for sound), and over 100 ms
  the layer seeks. Seeks land on the keyframe at or before the target and then
  decode forward, discarding frames, so the frame on screen is the right one.
* **GPU compositing, no CPU conversion.** Frames stay in YUV420P/NV12 and are
  sampled on the GPU (BT.709 limited and full range, in WGSL). Playback never
  converts to RGBA; the only CPU repacking is the explicit *Copy this frame*.
* **Hardware decode with a transparent fallback.** VA-API → Vulkan → VDPAU on
  Linux, D3D11VA → DXVA2 on Windows, VideoToolbox on macOS. If the driver
  refuses, the same file opens in software (frame + slice threading); the layer
  badge says which one is running, and says `converted` when a source format had
  to go through swscale.
* **Union timeline.** The session's length is the union of every layer's span;
  layers hold their first frame before they start and their last frame after they
  end, and playback stops when every layer is done. Unlink the timelines and each
  track gets its own transport.
* **One accent.** Black ground, white and `#A1A1AA` text, one `#FACC15` accent
  for live and selected state, 1 px borders, square corners, no shadows, no blur,
  no gradients.

## Requirements

| | |
|---|---|
| Rust | 1.95 or newer (`rust-version` in `Cargo.toml`) |
| FFmpeg | 8.x/9.x shared libraries with headers, `pkg-config` metadata and development packages |
| GPU | anything `wgpu` can drive (Vulkan, GL, Metal, DX12); software `llvmpipe` works |
| Audio | optional: without a sound device the player runs on its internal clock |

### Arch Linux (primary target)

```bash
sudo pacman -S ffmpeg libva vulkan-icd-loader alsa-lib \
               rust clang pkgconf base-devel
```

Check that the driver side is actually working before blaming the player:

```bash
vainfo                 # should list a driver and profiles
vulkaninfo --summary   # should list a device
```

Install the driver for your GPU if either is empty: `mesa` (Intel/AMD),
`intel-media-driver` (Intel Gen8+), `libva-intel-driver` (older Intel),
`nvidia-utils` (NVIDIA).

Then either build from a checkout:

```bash
cargo build --release
cargo test
./target/release/reaction-player
```

or install the package description in this repository:

```bash
cp PKGBUILD /tmp/reaction-player/ && cd /tmp/reaction-player && makepkg -si
```

### Windows (MSVC)

Install the Rust toolchain (`x86_64-pc-windows-msvc`), Visual Studo Build Tools
with the C++ workload, and FFmpeg through vcpkg:

```powershell
vcpkg install ffmpeg:x64-windows
setx VCPKG_ROOT C:\dev\vcpkg
cargo build --release
```

`ffmpeg-sys-next` finds FFmpeg through `FFMPEG_DIR` (the directory that contains
`include/` and `lib/`) or through `VCPKG_ROOT`. If neither is set the build stops
with a `pkg-config`-style error naming the library it could not find.

### macOS (Homebrew)

```bash
brew install ffmpeg pkg-config
cargo build --release
```

On Apple silicon the Homebrew prefix (`/opt/homebrew`) is picked up from
`pkg-config` automatically; if not, export
`PKG_CONFIG_PATH="$(brew --prefix ffmpeg)/lib/pkgconfig"`.

## FFmpeg version pairing

`ffmpeg-next`/`ffmpeg-sys-next` are bound to a **libavcodec major**, and the
bindings are unsafe Rust: a mismatch is not caught by the linker, it produces
wrong struct layouts at runtime. Pair them:

| `ffmpeg-next` | `ffmpeg-sys-next` | system FFmpeg | libavcodec |
|---|---|---|---|
| 9.1 | 9.1 | 9.x (Arch current) | 63 |
| 8.x | 8.x | 8.x | 62 |
| 7.x | 7.x | 7.x | 61 |

This repository pins `ffmpeg-next = "9.1"` / `ffmpeg-sys-next = "9.1"`. Arch ships
FFmpeg 9.x as of writing, so `pacman -S ffmpeg` matches. If your distribution
still ships FFmpeg 7.x, change **both** crate versions to `7.1` — not one of them.

The application prints the libavcodec version it linked against under
*Settings → Decoding*, and `cargo test` fails loudly if the shader and the
uniform layout disagree.

## Using it

**Home** is the only screen with a title treatment. *New session* opens the
layout dialog (side-by-side, grid, picture-in-picture either way round), *Demo*
generates two synthetic clips with `ffmpeg` and opens them, *Load session* lists
what is saved, *Presets* also leads to the Alignment Studio, *Settings* holds
decoding, transport and appearance preferences.

**Player.** The canvas keeps the session's aspect (16:9, 9:16, 4:3, 1:1, 21:9 or
fill the window). Drag a layer to move it, drag one of its eight handles to
resize it, double-click to bring it forward. Moves snap to the canvas edges, the
centre lines and the other layers' edges and centres, with 1 px guides.
Right-click a layer for *Copy this frame*, the z-order actions, crop sliders,
opacity, fit mode, crop, master audio, lock, visibility, *Reset transform* and
*Remove layer*. Layers can also be reordered by dragging in the layers panel, and
hidden and locked individually. *Lock layout* freezes all rectangles.

**Transport.** The scrubber is the union timeline with one span per layer; the
playhead, the per-layer spans and the timeline readout are all live. Next to it:
play/pause, the engine badge, link/unlink, master volume, ±1/±10/±100 ms
steppers, theater mode, full screen and the layers panel.

**Alignment Studio.** Pick A (reference) and B (other), step each side
independently by 1 or 10 frames, then `C` captures the last frames of both sides
with their exact `pts_us`. Compare with **Onion Skin** (a mix slider, default
50 %) or **Wipe Line** (a 1–2 px accent divider you can drag). The measured
difference is `offset_B = pts_B − pts_A`; *Apply to session* writes it into B.
With A's flash at 2.000 s and B's at 3.500 s, the offset becomes `+1500 ms`, and
at master time 2.000 s B shows its own 3.500 s — that is the whole sign
convention, and it has a unit test.

**Sessions** are saved under your data directory, with a thumbnail, and can be
renamed, duplicated, deleted, exported as `.rplay.json` and imported again. If a
file moved, the session card says how many layers are missing and the player
offers to relink.

## Keyboard

| Key | Action |
|---|---|
| `Space` | play / pause |
| `←` / `→` | one frame while paused, one seek step while playing, 1 % of the wipe in the studio |
| `Shift`+`←` / `→` | one seek step while paused, 10 % of the wipe in the studio |
| `,` / `.` | step one frame |
| `L` | link / unlink the timelines |
| `[` / `]` | move the selected layer down / up |
| `Shift`+`[` / `]` | send to back / bring to front |
| `T` | theater mode |
| `F` | full screen |
| `C` | copy the frame (player) / capture (studio) |
| `M` | mute the master audio layer |
| `P` | layers panel |
| `Esc` | close the topmost thing, then leave the player |
| `?` | this list |

Shortcuts are ignored while a text field has focus, and `Ctrl`/`Cmd`/`Alt`
combinations are left alone. The whole mapping is one pure function with tests.

## Data on disk

| What | Where |
|---|---|
| Preferences | `~/.config/reaction-player/preferences.json` |
| Sessions, presets, thumbnails, demo clips | `~/.local/share/reaction-player/` |
| Windows | `%APPDATA%\reaction-player`, `%LOCALAPPDATA%\reaction-player` |
| macOS | `~/Library/Application Support/reaction-player` |

Every JSON file carries a `"version"` field. *Settings → Local data → Clear local
data* deletes all of it after asking, and never touches your videos.

## Demo clips

The Demo button runs FFmpeg from Rust to generate:

```
source.mp4     testsrc2, 1280x720@30, 8 s, white flash in the first 200 ms
reaction.mp4   flat background, same size, white flash at 1500 ms
```

The demo session ships with the reaction offset at `+1_500_000 µs`, so the two
flashes line up — a visible proof that sync works. The same commands live in
`scripts/make-demo-clips.sh` if you want to generate them ahead of time or on a
machine without a GPU. If `ffmpeg` is not on `PATH`, the app says so and tells you
which package to install; it does not fail silently.

## How sync actually works

```
target_pts_us(layer) = master_clock_us + layer.offset_us
layer start          = -offset_us
layer end            = duration_us - offset_us
```

The master clock is written by the audio callback (`cpal`) as
`frames_written − output_latency`, and the UI interpolates between callbacks with
an `Instant`, bounded to 100 ms so a stalled device cannot run time away. Without
audio the same clock runs on `Instant` alone.

Per tick, per layer, the frame whose `pts` is at or before the target is put on
screen; a frame that is too new is kept for the next tick instead of being
dropped. Drift is classified exactly as above, seeks are rate-limited per layer
with hysteresis, and every seek bumps an `epoch` so frames decoded for the old
position can never reach the screen.

Audio is per track: one SPSC ring buffer per layer, written by that layer's audio
worker and mixed in one device callback with per-track volume and a master
volume. A seek bumps the ring's epoch and the mixer discards what is left.
*Keep the sound going* hands the sound to the highest active layer when the
master audio layer has no footage at the playhead.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `ffmpeg-sys-next` build error naming `avcodec` | headers or `pkg-config` files missing | install `ffmpeg` (Arch, which includes headers) or `libavcodec-dev` etc. |
| Build fails inside FFmpeg bindings with odd type errors | crate major ≠ system FFmpeg major | pair them, see the table above |
| A layer shows `software` | driver, device or format refused | check `vainfo`; that file may simply not be supported by the driver — software playback is not a failure |
| A layer shows `converted` | source is not 8-bit 4:2:0 (10-bit, 4:4:4, …) | expected; playback goes through swscale |
| No sound | no output device | video still plays on the internal clock; check *Settings* for the audio status |
| Window opens black, nothing plays | wgpu cannot get a surface | try another backend: `WGPU_BACKEND=gl` (or `vulkan`) |
| Everything is one frame late after a seek | a source with B-frames and no `pts` | the player falls back to `best_effort_timestamp`; remux with `-fflags +genpts` if it matters |

## What has been verified, and what has not

Honesty first, because this matters more than a badge:

* `cargo check --all-targets` and `cargo test --all-targets` run in CI on Arch
  Linux inside the `archlinux:latest` container (see
  `.github/workflows/ci.yml`), against the system FFmpeg. The full compiler
  output is committed back to the branch as `ci-log.txt`.
* CI is headless. **Hardware decoding, the audio device and the actual GPU
  compositing are not exercised there** — no driver, no sound card, no display.
  Those paths are written, documented and unit-tested where they can be, but they
  have not been run end to end on real hardware by the author of this build.
* The CPU-side picture path (BT.709 YUV → RGBA, crops, strides) and every piece
  of timeline, geometry, snapping, drift and alignment arithmetic **are** unit
  tested, including YUV420P and NV12 with `stride != width`.

## Layout of the source

```
src/model     sessions, layers, geometry, presets, preferences, persistence
src/engine    probing, decoding, seeking, audio, master clock, the runtime
src/gpu       the wgpu compositor, the shader, the CPU capture path
src/ui        theme, widgets, toasts, shortcuts, screens
src/app.rs    the eframe application and everything that ties it together
```

No module contains a `todo!()`, an `unimplemented!()`, or a placeholder that
pretends to work.

## Licence

MIT OR Apache-2.0.

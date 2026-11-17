#!/usr/bin/env bash
# Generate the two demo clips by hand.
#
# The app does this itself on first run (src/engine/demo.rs), so you only need
# this script when FFmpeg refuses to cooperate from inside the app, or when you
# want the clips in a specific directory.
#
#   ./scripts/make-demo-clips.sh                 # into the data directory
#   ./scripts/make-demo-clips.sh /tmp/demo       # into /tmp/demo
#
# The argument list must stay in step with `source_args` / `reaction_args` in
# src/engine/demo.rs -- the constants below are copied from there, and the
# unit tests in that file assert the same flash times.

set -euo pipefail

# --- must match src/engine/demo.rs ---------------------------------------
DURATION_S=10
FPS=30
BEEP_HZ=1000
REACTION_FLASH_MS=3000
SOURCE_FLASH_MS=1500 # 1500 ms before the reaction's flash
FLASH_LEN_MS=200
SOURCE_FILE="source-testsrc2.mp4"
REACTION_FILE="reaction-flash.mp4"
# -------------------------------------------------------------------------

out_dir="${1:-${XDG_DATA_HOME:-$HOME/.local/share}/reaction-player/demo}"

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "ffmpeg not found on PATH. Install it first (Arch: sudo pacman -S ffmpeg)." >&2
    exit 1
fi

mkdir -p "$out_dir"

# Drawbox windows, formatted the same way the Rust code formats them (three
# decimals), so a diff between the two cannot drift silently.
src_start=$(awk -v ms="$SOURCE_FLASH_MS" 'BEGIN { printf "%.3f", ms / 1000 }')
src_end=$(awk -v ms="$((SOURCE_FLASH_MS + FLASH_LEN_MS))" 'BEGIN { printf "%.3f", ms / 1000 }')
rxn_start=$(awk -v ms="$REACTION_FLASH_MS" 'BEGIN { printf "%.3f", ms / 1000 }')
rxn_end=$(awk -v ms="$((REACTION_FLASH_MS + FLASH_LEN_MS))" 'BEGIN { printf "%.3f", ms / 1000 }')

echo "Writing demo clips to $out_dir"

# Source: testsrc2 with a white flash at 1.5 s.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=1280x720:rate=${FPS}:duration=${DURATION_S}" \
    -f lavfi -i "sine=frequency=${BEEP_HZ}:duration=${DURATION_S}:sample_rate=48000" \
    -vf "drawbox=x=0:y=0:w=iw:h=ih:color=white@1.0:t=fill:enable='between(t,${src_start},${src_end})',format=yuv420p" \
    -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p \
    -c:a aac -b:a 128k -shortest -movflags +faststart \
    "$out_dir/$SOURCE_FILE"

# Reaction: flat dark background with an accent flash at 3.0 s, i.e. exactly
# 1500 ms after the source's flash. That gap is DEMO_REACTION_OFFSET_US.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "color=c=0x101014:size=960x540:rate=${FPS}:duration=${DURATION_S}" \
    -f lavfi -i "sine=frequency=${BEEP_HZ}:duration=${DURATION_S}:sample_rate=48000" \
    -vf "drawbox=x=0:y=0:w=iw:h=ih:color=0xFACC15@1.0:t=fill:enable='between(t,${rxn_start},${rxn_end})',format=yuv420p" \
    -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p \
    -c:a aac -b:a 128k -shortest -movflags +faststart \
    "$out_dir/$REACTION_FILE"

echo
echo "Done:"
ls -lh "$out_dir/$SOURCE_FILE" "$out_dir/$REACTION_FILE"
echo
echo "The reaction flash is ${REACTION_FLASH_MS} ms in; the source flash is ${SOURCE_FLASH_MS} ms in."
echo "A correct session sets offset_B = $(((REACTION_FLASH_MS - SOURCE_FLASH_MS) * 1000)) µs."

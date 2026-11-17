#!/usr/bin/env bash
# Build the demo clips the same way the app does, so they can be generated ahead
# of time (for a screenshot, a test machine, or to skip the first-run wait).
#
#   scripts/make-demo-clips.sh [output-dir]
#
# The default output directory is the app's data directory, which is where the
# Demo button looks:
#
#   ${XDG_DATA_HOME:-~/.local/share}/reaction-player/demo
#
# The two clips are both 8 s, 1280x720, 30 fps, H.264 + AAC:
#
#   * source.mp4   - testsrc2 with a white flash in the first 200 ms
#   * reaction.mp4 - a flat background with the same flash 1500 ms in
#
# The flash is the alignment landmark: the demo session ships with the reaction
# offset at +1_500_000 us so the two flashes line up.

set -euo pipefail

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
out_dir="${1:-$data_home/reaction-player/demo}"

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "error: ffmpeg is not installed." >&2
    echo "       sudo pacman -S ffmpeg   (Arch)" >&2
    echo "       brew install ffmpeg     (macOS)" >&2
    exit 1
fi

mkdir -p "$out_dir"

echo "writing $out_dir/source.mp4"
ffmpeg -y -nostdin -hide_banner -loglevel error \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=8" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=8" \
    -vf "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(t,0.2)'" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p \
    -c:a aac -b:a 128k -shortest -movflags +faststart \
    "$out_dir/source.mp4"

echo "writing $out_dir/reaction.mp4"
ffmpeg -y -nostdin -hide_banner -loglevel error \
    -f lavfi -i "color=c=0x0A0A0A:size=1280x720:rate=30:duration=8" \
    -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=8" \
    -vf "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(t,1.5, 1.7)'" \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p \
    -c:a aac -b:a 128k -shortest -movflags +faststart \
    "$out_dir/reaction.mp4"

ls -l "$out_dir"/*.mp4
echo "done: open the app and press Demo, or load the two files into a new session"
echo "      with the reaction's offset set to +1.500 s."

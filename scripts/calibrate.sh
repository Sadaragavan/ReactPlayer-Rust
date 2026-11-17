#!/usr/bin/env bash
# The calibration workflow, end to end, from the command line.
#
# The GUI does exactly this with the mouse; this script exists so the same
# numbers can be reproduced in a bug report, and so the timing maths can be
# checked without a GPU, a window, or an audio device.
#
#   ./scripts/calibrate.sh --a source.mp4 --b reaction.mp4 \
#       --a-frame 00:00:12.480 --b-frame 00:00:14.100
#
# It reports three things:
#
#   pts_A, pts_B   the exact presentation timestamps of the two frames, read
#                  from the container with ffprobe (microsecond precision)
#   delta          pts_B - pts_A, i.e. the offset that makes the two frames
#                  simultaneous. This is what "Apply to B" writes as
#                  offset_B in the session file.
#
# Frame-accurate caveat: ffprobe reports *decoded* timestamps, while the player
# seeks on the index and then decodes-and-discards until it reaches the target.
# On a file with B-frames the two agree because both are presentation-order
# timestamps from the same demuxer; on a broken index they can differ, which is
# why the GUI also shows the frame it actually landed on.

set -euo pipefail

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

A=""
B=""
A_FRAME=""
B_FRAME=""
TOLERANCE="0.5"

while [ $# -gt 0 ]; do
    case "$1" in
        --a) A="$2"; shift 2 ;;
        --b) B="$2"; shift 2 ;;
        --a-frame) A_FRAME="$2"; shift 2 ;;
        --b-frame) B_FRAME="$2"; shift 2 ;;
        --tolerance) TOLERANCE="$2"; shift 2 ;;
        -h | --help) usage 0 ;;
        *) echo "unknown argument: $1" >&2; usage 2 ;;
    esac
done

[ -n "$A" ] && [ -n "$B" ] || { echo "need --a and --b" >&2; usage 2; }
command -v ffprobe >/dev/null 2>&1 || {
    echo "ffprobe not found; install FFmpeg" >&2
    exit 1
}

for f in "$A" "$B"; do
    [ -r "$f" ] || { echo "cannot read $f" >&2; exit 1; }
done

# Timestamp of the frame nearest to $2 (a position or a timestamp), in
# microseconds. Uses the video stream only and asks ffprobe for the frames
# around the requested time, then picks the closest.
frame_pts_us() {
    local file=$1
    local target=$2
    ffprobe -v error -select_streams v:0 \
        -show_entries frame=pts_time,best_effort_timestamp_time \
        -of csv=p=0 \
        -read_intervals "${target}%+#2" \
        "$file" |
        awk -F, -v want="$target" '
        BEGIN { best = ""; bestd = 1e9 }
        {
            for (i = 1; i <= NF; i++) {
                if ($i == "") continue
                d = $i - want
                if (d < 0) d = -d
                if (d < bestd) { bestd = d; best = $i }
            }
        }
        END {
            if (best == "") exit 1
            printf "%.0f\n", best * 1000000
        }'
}

stream_summary() {
    local file=$1
    ffprobe -v error -select_streams v:0 \
        -show_entries stream=width,height,r_frame_rate,avg_frame_rate,nb_frames,codec_name \
        -of default=noprint_wrappers=1 "$file"
}

say() { printf '\033[1m%s\033[0m\n' "$*"; }

say "== A: $A"
stream_summary "$A"
say "== B: $B"
stream_summary "$B"

# Accept either a wall-clock timestamp (HH:MM:SS.mmm) or plain seconds.
to_seconds() {
    local t=$1
    case "$t" in
        *:*:*) awk -F: '{ printf "%.6f\n", $1*3600 + $2*60 + $3 }' <<<"$t" ;;
        *:*) awk -F: '{ printf "%.6f\n", $1*60 + $2 }' <<<"$t" ;;
        *) printf '%.6f\n' "$t" ;;
    esac
}

if [ -n "$A_FRAME" ] && [ -n "$B_FRAME" ]; then
    a_sec=$(to_seconds "$A_FRAME")
    b_sec=$(to_seconds "$B_FRAME")

    a_us=$(frame_pts_us "$A" "$a_sec")
    b_us=$(frame_pts_us "$B" "$b_sec")
    delta=$((b_us - a_us))

    say ""
    say "pts_A  = $a_us µs"
    say "pts_B  = $b_us µs"
    say "delta  = $delta µs"

    printf 'offset_B = %d  # add this to B so both frames line up\n' "$delta"

    # Sanity check: a delta larger than the tolerance is usually a mis-typed
    # timestamp rather than a real offset, so say so instead of printing a
    # confident number.
    tol_us=$(awk -v t="$TOLERANCE" 'BEGIN { printf "%.0f", t * 1000000 }')
    abs_delta=${delta#-}
    if [ "$abs_delta" -gt "$tol_us" ]; then
        say ""
        printf '\033[33mNote: |delta| exceeds %s s. Check that the two timestamps\n' "$TOLERANCE"
        printf 'really are the pair of frames you mean.\033[0m\n'
    fi
else
    say ""
    echo "Pass --a-frame and --b-frame to compute an offset."
fi

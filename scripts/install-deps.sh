#!/usr/bin/env bash
# Install everything reaction-player needs to build, on the distro this script
# recognises. Safe to re-run.
#
#   ./scripts/install-deps.sh            # install
#   ./scripts/install-deps.sh --check    # only report what is missing
#
# Why a script: the Rust side is only half the story. Building this crate needs
# a C toolchain (bindgen compiles a shim against FFmpeg's headers), the FFmpeg
# development headers for the *matching major version*, pkg-config, and the
# X11/Wayland headers eframe's windowing stack links against. Getting one of
# those wrong produces a wall of linker errors that says nothing about the real
# cause, so the script checks the version pairing explicitly.

set -euo pipefail

CHECK_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --check) CHECK_ONLY=1 ;;
        -h | --help)
            sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

# ffmpeg-next's major version tracks FFmpeg's: 9.x needs libavcodec >= 63.1.
REQUIRED_FFMPEG_MAJOR=9

say() { printf '\033[1m%s\033[0m\n' "$*"; }
warn() { printf '\033[33m%s\033[0m\n' "$*" >&2; }
die() {
    printf '\033[31m%s\033[0m\n' "$*" >&2
    exit 1
}

detect_distro() {
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        echo "${ID:-unknown}"
    else
        echo unknown
    fi
}

# ---------------------------------------------------------------- presence
have() { command -v "$1" >/dev/null 2>&1; }

missing=()
have rustc || missing+=("rustc (install via https://rustup.rs)")
have cargo || missing+=("cargo")
have pkg-config || missing+=("pkg-config")
have cc || missing+=("a C compiler (cc)")
have clang || missing+=("clang (bindgen's libclang)")

ffmpeg_pc_version() {
    pkg-config --modversion libavcodec 2>/dev/null || echo none
}

ffmpeg_major=$(ffmpeg_pc_version)
if [ "$ffmpeg_major" = none ]; then
    missing+=("FFmpeg ${REQUIRED_FFMPEG_MAJOR}.x development headers (libavcodec.pc)")
else
    case "$ffmpeg_major" in
        "$REQUIRED_FFMPEG_MAJOR".*) : ;;
        *) warn "libavcodec is $ffmpeg_major, but ffmpeg-next 9.x needs $REQUIRED_FFMPEG_MAJOR.x" ;;
    esac
fi

if [ "$CHECK_ONLY" -eq 1 ]; then
    if [ "${#missing[@]}" -eq 0 ]; then
        say "All build dependencies are present."
        exit 0
    fi
    warn "Missing:"
    printf '  - %s\n' "${missing[@]}" >&2
    exit 1
fi

# ---------------------------------------------------------------- install
distro=$(detect_distro)
say "Detected distro: $distro"

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif have sudo; then
        sudo "$@"
    else
        die "need root (or sudo) to install packages"
    fi
}

case "$distro" in
    arch | archarm | endeavouros | manjaro | garuda)
        say "Installing Arch packages (PKGBUILD prefers ./PKGBUILD; this is for a plain dev box)."
        as_root pacman -S --needed --noconfirm \
            base-devel rust pkgconf clang ffmpeg \
            libx11 libxcursor libxi libxrandr libxkbcommon wayland \
            vulkan-icd-loader alsa-lib
        ;;
    debian | ubuntu | linuxmint | pop)
        say "Installing Debian/Ubuntu packages."
        # FFmpeg 9.x is not in Debian 12 / Ubuntu 24.04. The Rust side still
        # compiles against the headers that are present, but check the pairing
        # afterwards with --check and build FFmpeg from source if needed.
        as_root apt-get update
        as_root apt-get install -y --no-install-recommends \
            build-essential pkg-config clang libclang-dev \
            libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
            libavfilter-dev libavdevice-dev \
            libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
            libxkbcommon-dev libwayland-dev libvulkan-dev \
            libasound2-dev
        ;;
    fedora)
        say "Installing Fedora packages."
        as_root dnf install -y \
            gcc gcc-c++ make pkgconf-pkg-config clang clang-devel \
            ffmpeg-devel \
            libX11-devel libXcursor-devel libXi-devel libXrandr-devel \
            libxkbcommon-devel wayland-devel vulkan-loader-devel \
            alsa-lib-devel
        ;;
    *)
        die "Unrecognised distro '$distro'. Install manually: rustup, pkg-config, clang,
a C compiler, FFmpeg ${REQUIRED_FFMPEG_MAJOR}.x headers, X11/Wayland dev headers,
Vulkan loader, and alsa-lib."
        ;;
esac

say "Done. Verify with: ./scripts/install-deps.sh --check"

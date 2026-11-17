# Maintainer: Sadaragavan <noreply@github.com>
#
# Arch package for reaction-player.
#
#   cd into a clean directory, put this file there, then:
#     makepkg -si
#
# Two things worth knowing before reading the rest:
#
#   1. *FFmpeg pairing.* reaction-player depends on ffmpeg-next 9.x, whose
#      major version tracks FFmpeg's. Arch's `ffmpeg` package is on whatever
#      major is current; if it has moved past 9.x, this PKGBUILD will fail at
#      build time with a bindgen/pkg-config error rather than produce a binary
#      that decodes garbage. In that case build against `ffmpeg-9` from the
#      AUR (or downgrade), which is the smaller evil compared to shipping a
#      silently-mismatched libavcodec ABI.
#   2. *Visibility.* The GUI is a normal window; the player never grabs the
#      pointer, so no special permissions are needed. Audio goes through the
#      user's session via ALSA/PipeWire, no extra groups.

pkgname=reaction-player
pkgver=0.1.0
pkgrel=1
pkgdesc='Frame-accurate multi-stream video reaction player and alignment tool'
arch=('x86_64' 'aarch64')
url='https://github.com/Sadaragavan/ReactPlayer-Rust'
license=('MIT OR Apache-2.0')
# Runtime: the shared libraries the binary links against. The dev packages are
# only needed to build and are listed in makedepends.
depends=(
    'alsa-lib'
    'ffmpeg'
    'gcc-libs'
    'glibc'
    'libva'
    'libx11'
    'libxcursor'
    'libxi'
    'libxkbcommon'
    'libxrandr'
    'vulkan-icd-loader'
    'wayland'
)
# The drivers themselves are the user's choice: VA-API on Intel/AMD, VDPAU or
# Vulkan video on NVIDIA. Without one of them playback still works, in software,
# and the layer badge says so.
optdepends=(
    'mesa: VA-API and Vulkan drivers for Intel and AMD GPUs'
    'intel-media-driver: VA-API on Intel Gen8 and newer'
    'libva-intel-driver: VA-API on older Intel GPUs'
    'nvidia-utils: VDPAU and Vulkan video on NVIDIA'
)
# Build: Rust toolchain, a C toolchain for bindgen, pkg-config to find FFmpeg,
# and clang because ffmpeg-sys-next generates bindings at build time.
makedepends=(
    'cargo'
    'clang'
    'rust'
    'pkgconf'
)
# `sessions/`, `preferences.json` and the demo media live under
# XDG_DATA_HOME/reaction-player and XDG_CONFIG_HOME/reaction-player, so no
# install-time state is created for the user.
options=('!lto')
source=("$pkgname-$pkgver.tar.gz::$url/archive/v$pkgver.tar.gz")
sha256sums=('SKIP')

prepare() {
    cd "$pkgname-$pkgver"
    # Cargo's lockfile vendoring is done by the build function below; nothing
    # to patch here yet. Kept explicit so a future patch step has an obvious
    # home.
    :
}

build() {
    cd "$pkgname-$pkgver"
    # `--locked` is deliberately absent: the repository does not commit a
    # Cargo.lock yet. Once it does, add `--locked` so a rebuild cannot drift
    # onto newer crate versions than the ones this release was tested with.
    cargo build --release
}

check() {
    cd "$pkgname-$pkgver"
    cargo test --release
}

package() {
    cd "$pkgname-$pkgver"
    install -Dm755 "target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
    if [ -f LICENSE-MIT ]; then
        install -Dm644 LICENSE-MIT "$pkgdir/usr/share/licenses/$pkgname/LICENSE-MIT"
    fi
    if [ -f LICENSE-APACHE ]; then
        install -Dm644 LICENSE-APACHE "$pkgdir/usr/share/licenses/$pkgname/LICENSE-APACHE"
    fi
    # Desktop entry: the app_id in main.rs is `reaction-player`, which is what
    # Wayland's app_id and X11's WM_CLASS report, so the basename has to match
    # for the icon and the window to be associated.
    install -Dm644 /dev/stdin "$pkgdir/usr/share/applications/$pkgname.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=reaction-player
Comment=Frame-accurate multi-stream reaction player and alignment tool
Exec=reaction-player
Terminal=false
Categories=AudioVideo;Video;Player;
Keywords=video;reaction;alignment;compare;
StartupWMClass=reaction-player
EOF
}

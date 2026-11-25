#!/bin/bash
# Cross-build packaging/linux-nano (linux-nano + linux-nano-headers, aarch64) on an x86_64
# Ubuntu 24.04 / WSL2 host with makepkg and gcc 14 for aarch64: minutes instead of hours on the Nano.
#
#   sudo bash scripts/cross-build-linux-nano.sh --deps      # once: apt packages
#   bash scripts/cross-build-linux-nano.sh [OUT_DIR]       # as a normal user (makepkg refuses root)
#
# The packages land in OUT_DIR (default ./out/linux-nano). The headers' host tools (modpost,
# fixdep, ...) are x86 binaries: on the Nano, rebuild them once before building nvgpu-nano:
#   make -C /usr/lib/modules/<kver>/build scripts_basic scripts   (needs base-devel)
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
die() { echo "error: $*" >&2; exit 1; }

if [[ ${1:-} == --deps ]]; then
  [[ $EUID -eq 0 ]] || die "--deps needs root"
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y makepkg pacman-package-manager fakeroot \
    gcc-14-aarch64-linux-gnu binutils-aarch64-linux-gnu build-essential bc bison flex kmod \
    libssl-dev libelf-dev python3 git xz-utils zstd libarchive-tools file device-tree-compiler cpio rsync
  exit 0
fi
[[ $EUID -ne 0 ]] || die "run as a normal user (makepkg refuses root)"

OUT=$(realpath -m "${1:-$here/out/linux-nano}")
W=${WORK:-/var/tmp/linux-nano-cross}
mkdir -p "$W/bin" "$W/src" "$OUT"

# Cross toolchain under the names the kernel Makefile uses (CROSS_COMPILE=aarch64-linux-gnu-).
for t in gcc cpp; do ln -sf "$(command -v aarch64-linux-gnu-$t-14)" "$W/bin/aarch64-linux-gnu-$t"; done
# The headers package strips its build tools with plain `strip`: x86 host tools and aarch64 files.
cat > "$W/bin/strip" <<'S'
#!/bin/sh
aarch64-linux-gnu-strip "$@" 2>/dev/null || /usr/bin/strip "$@"
S
chmod +x "$W/bin/strip"

cat > "$W/makepkg.conf" <<C
source /etc/makepkg.conf
CARCH=aarch64
CHOST=aarch64-linux-gnu
MAKEFLAGS="-j$(nproc)"
PKGEXT='.pkg.tar.zst'
COMPRESSZST=(zstd -c -T0 -19 -)
SRCDEST="$W/src"
PKGDEST="$OUT"
BUILDDIR="$W/build"
OPTIONS=(!strip !debug !lto)
C

rm -rf "$W/pkg" && cp -a "$here/packaging/linux-nano" "$W/pkg"
# A Windows checkout may have CRLF line ends: makepkg refuses them and git apply fails on them.
find "$W/pkg" -type f -exec sed -i 's/\r$//' {} +
cd "$W/pkg"
# no user or host name of the build machine in the kernel
export KBUILD_BUILD_USER=nano KBUILD_BUILD_HOST=j202
export PATH="$W/bin:$PATH" ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
makepkg --config "$W/makepkg.conf" --nodeps --cleanbuild --force --noconfirm
ls -l "$OUT"

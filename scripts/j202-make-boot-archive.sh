#!/bin/bash
# reComputer J1020 v2 (J202): build j202-boot-VERSION.tar.zst, the boot files of a release.
# Run on the board, as root, from a checkout, with the release's linux-nano installed:
#   bash scripts/j202-make-boot-archive.sh VERSION [OUT_DIR]
#
# Nothing in it belongs to one board or to NVIDIA: the initramfs is built with the project's loading
# screen and without the NVDEC firmware (L4T's licence; it is read from the disk later), the device
# trees have no memory timings (j202-install-boot.sh takes them from the board), the menu is written
# at installation.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
version=${1:?usage: $0 VERSION [OUT_DIR]}
dest=$(realpath -m "${2:-.}")
die() { echo "error: $*" >&2; exit 1; }
[[ $EUID -eq 0 ]] || die "run as root"
kver=$(pacman -Q linux-nano | cut -d' ' -f2)-nano-ARCH
theme=$here/omarchy/nano/files/usr/share/plymouth/themes/jetson-nano
[[ -f $theme/splash.png ]] || die "no $theme/splash.png"
pacman -Qkk linux-nano 2>/dev/null | grep -q ' 0 altered files' || die "the installed linux-nano is not the packaged one"

tmp=$(mktemp -d /var/tmp/j202-archive.XXXXXX); trap 'rm -rf "$tmp"' EXIT
a=$tmp/j202-boot
mkdir -p "$a/boot/j202" "$a/scripts" "$a/omarchy/nano/files/usr/share/plymouth/themes" "$a/images/mainline-kernel/b01"

echo "$kver" > "$a/boot/kver"
install -m644 /boot/Image-nano "$a/boot/"

# the initramfs, in a namespace: the project's pictures, no NVDEC firmware; this board keeps its own
mkdir -p "$tmp/up" "$tmp/wk" "$tmp/themes/jetson-nano"
cp "$theme"/* "$tmp/themes/jetson-nano/"
unshare -m bash -euc '
  mount --bind "$1/themes/jetson-nano" /usr/share/plymouth/themes/jetson-nano
  mount -t overlay overlay -o lowerdir=/usr/lib/firmware,upperdir="$1/up",workdir="$1/wk" /usr/lib/firmware
  rm -f /usr/lib/firmware/nvidia/tegra210/nvdec.bin*
  mkinitcpio -k "$2" -g "$1/initramfs.img"
' _ "$tmp" "$kver" > "$tmp/mkinitcpio.log" 2>&1 || { tail -20 "$tmp/mkinitcpio.log"; die "mkinitcpio failed"; }
install -m644 "$tmp/initramfs.img" "$a/boot/initramfs-linux-nano.img"
lsinitcpio -l "$a/boot/initramfs-linux-nano.img" > "$tmp/list"
if grep -q 'nvdec.bin' "$tmp/list"; then die "the NVDEC firmware is in the initramfs"; fi
for need in splash.png tegra-drm.ko; do grep -q "$need" "$tmp/list" || die "no $need in the initramfs"; done
grep -q /nvme.ko "$tmp/list" "/usr/lib/modules/$kver/modules.builtin" || die "no NVMe driver at boot"
mkdir "$tmp/x" && (cd "$tmp/x" && lsinitcpio -x "$a/boot/initramfs-linux-nano.img" >/dev/null 2>&1)
cmp -s "$tmp/x/usr/share/plymouth/themes/jetson-nano/splash.png" "$theme/splash.png" || die "not the project's picture in the initramfs"
awk -F: 'length($2) > 2 { exit 1 }' "$tmp/x/etc/shadow" || die "a password in the initramfs"

# device trees without timings
d=$a/boot/j202/nano-j202.dtb
install -m644 /boot/dtbs-nano/nvidia/tegra210-p3448-0002-j202.dtb "$d"
fdtput "$d" /i2c@7000d000/pmic@3c system-power-controller
bash "$here/images/mainline-kernel/fix-emmc-dtb.sh" "$d"
bash "$here/images/mainline-kernel/fix-video-dtb.sh" "$d"
bash "$here/images/mainline-kernel/make-nano-dtb.sh" "$d" "$a/boot/j202/nano-j202-cuda.dtb"
[[ -z $(fdtget -l "$d" /external-memory-controller@7001b000 2>/dev/null) ]] || die "memory timings in the device tree"

cp "$here/scripts/j202-install-boot.sh" "$a/scripts/"
cp "$here"/images/mainline-kernel/{make-j202-nano-dtb,make-nano-dtb,fix-emmc-dtb,fix-video-dtb}.sh "$a/images/mainline-kernel/"
cp "$here/images/mainline-kernel/b01/nvmap-carveouts.dtsi" "$a/images/mainline-kernel/b01/"
cp "$here/omarchy/nano/bootlogo.py" "$here/omarchy/nano/bootlogo-j202.png" "$a/omarchy/nano/"
cp -r "$theme" "$a/omarchy/nano/files/usr/share/plymouth/themes/"
cat > "$a/README" <<EOF
Boot files of jetson-nano-omarchy for the reComputer J1020 v2 (J202), $version, kernel $kver.

Install the release's packages first, then, on the board, as root:
  bash scripts/j202-install-boot.sh --dry-run     shows what it would do
  bash scripts/j202-install-boot.sh --default nano-cuda
Add --logo to replace the first picture too. The kernel's sources are in the release's tag
(packaging/linux-nano). The header of the script tells the rest.
EOF
find "$a" -type d -exec chmod 755 {} + ; find "$a" -type f -exec chmod 644 {} +
(cd "$a" && find . -type f ! -name SHA256SUMS | sort | xargs sha256sum > SHA256SUMS)
mkdir -p "$dest"
tar --owner=0 --group=0 --numeric-owner --sort=name -C "$tmp" -c j202-boot | zstd -q -19 -T0 -f -o "$dest/j202-boot-$version.tar.zst"
ls -l "$dest/j202-boot-$version.tar.zst"

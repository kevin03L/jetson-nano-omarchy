#!/bin/bash
# reComputer J1020 v2 (J202): install the boot files of the project from the release's
# j202-boot-*.tar.zst, or from a checkout of this repository: kernel, initramfs, device trees, boot
# menu entries, loading screen. Run it on the board, as root, with linux-nano and nvgpu-nano installed:
#
#   tar xf j202-boot-*.tar.zst && cd j202-boot && bash scripts/j202-install-boot.sh [options]
#
#   --logo      also replace the first picture (cboot's, eMMC partition BMP): the old one is kept in /root
#   --default L make entry L (nano-cuda, nano, nano-oc...) the default one; otherwise it is kept
#   --dry-run   build everything in a directory and show it, change nothing
#
# - What is replaced is saved first in /root/boot-before-DATE.
# - The memory timings in the device trees are the ones cboot trained on THIS board
#   (make-j202-nano-dtb.sh). When it cannot read them, the device trees of the archive are used,
#   without timings: the memory then stays at the bootloader's speed. Run the script again later.
# - Entries of the menu that are not the project's (mainline, nvme, nano-prev...) are left alone.
# - nano-oc is beyond the clocks NVIDIA guarantees: it is added, never chosen by itself.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
LOGO=0 DRY=0 DEFAULT=
while (($#)); do
  case $1 in
    --logo) LOGO=1 ;;
    --dry-run) DRY=1 ;;
    --default) DEFAULT=${2:?--default needs an entry}; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
say() { printf '>> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"
tr '\0' '\n' < /proc/device-tree/compatible | grep -qx 'seeed,recomputer-j1020-v2' \
  || die "this is not a reComputer J1020 v2 booted with a J202 device tree"
for c in dtc fdtget fdtput findmnt; do command -v $c >/dev/null || die "needs $c (pacman -S dtc util-linux)"; done
BOOT=/boot EXT=/boot/extlinux/extlinux.conf
[[ -f $EXT ]] || die "no $EXT: is /boot mounted?"
[[ $(findmnt -no TARGET -T /boot) != / || $(findmnt -no SOURCE /) == /dev/mmcblk0p1 ]] \
  || die "/boot is not on the eMMC (not mounted?)"

# the kernel of the archive, or the installed one when run from a checkout
if [[ -f $here/boot/Image-nano ]]; then
  kver=$(cat "$here/boot/kver")
else
  kver=$(pacman -Q linux-nano | cut -d' ' -f2)-nano-ARCH
fi
[[ -d /usr/lib/modules/$kver/kernel ]] || die "the modules of $kver are not installed: install the release's linux-nano first"
[[ -f /usr/lib/modules/$kver/extramodules/nvgpu.ko || -f /usr/lib/modules/$kver/extramodules/nvgpu.ko.zst ]] \
  || say "warning: no nvgpu module for $kver, the nano-cuda and nano-oc entries need nvgpu-nano"

root=$(findmnt -no SOURCE /) && partuuid=$(lsblk -no PARTUUID "$root") && fstype=$(findmnt -no FSTYPE /)
[[ -n $partuuid ]] || die "no PARTUUID for the root $root"

out=$(mktemp -d /var/tmp/j202-boot.XXXXXX)
[[ $DRY == 1 ]] || trap 'rm -rf "$out"' EXIT
mkdir -p "$out/j202"

say "kernel $kver, root $root ($partuuid)"
if [[ -f $here/boot/Image-nano ]]; then
  (cd "$here" && sha256sum -c --quiet SHA256SUMS) || die "the archive is damaged"
  cp "$here/boot/Image-nano" "$here/boot/initramfs-linux-nano.img" "$out/"
fi

say "device trees"
mk=$here/images/mainline-kernel
base=$here/boot/j202
if bash "$mk/make-j202-nano-dtb.sh" /boot/dtbs-nano/nvidia/tegra210-p3448-0002-j202.dtb "$out/j202/nano-j202.dtb"; then
  say "memory timings of this board taken from cboot"
elif [[ -f $base/nano-j202.dtb ]]; then
  say "WARNING: cboot's memory timings cannot be read now: device tree without timings"
  cp "$base/nano-j202.dtb" "$out/j202/"
else
  die "no device tree"
fi
bash "$mk/make-nano-dtb.sh" "$out/j202/nano-j202.dtb" "$out/j202/nano-j202-cuda.dtb"
for f in nano-j202 nano-j202-cuda; do
  [[ $(fdtget -t u "$out/j202/$f.dtb" /mmc@700b0600 max-frequency) == 52000000 ]] || die "$f.dtb: eMMC not set"
  fdtget -p "$out/j202/$f.dtb" /pmc@7000e400/powergates/nvdec | grep -qx power-domains || die "$f.dtb: NVDEC not set"
done

say "boot menu"
args="root=PARTUUID=$partuuid rw rootwait rootfstype=$fstype console=ttyS0,115200n8"
tail="loglevel=7 fbcon=map:0 net.ifnames=0 clk_ignore_unused pd_ignore_unused panic=10"
cuda="modprobe.blacklist=nouveau nano.mode=cuda cma=256M"
splash="splash plymouth.ignore-serial-consoles vt.global_cursor_default=0 logo.nologo"
oc="nvgpu.gpu_max_khz=1152000 clk_tegra124_dfll_fcpu.max_khz=1785000 nano.cpu_max_khz=1785000"
entry() { # label title device-tree arguments
  printf '\nLABEL %s\n      MENU LABEL %s\n      LINUX /boot/Image-nano\n      INITRD /boot/initramfs-linux-nano.img\n' "$1" "$2"
  printf '      FDT /boot/j202/%s.dtb\n      APPEND %s\n' "$3" "$4"
}
{
  awk '$1 == "LABEL" { p = ($2 == "nano-plain" || $2 == "nano-cuda" || $2 == "nano" || $2 == "nano-oc") } !p' "$EXT" | cat -s
  entry nano-plain "linux-nano, text, no GPU driver choice" nano-j202 "$args console=tty0 $tail cma=256M"
  entry nano-cuda "linux-nano: desktop and CUDA, stock clocks" nano-j202-cuda "$args $tail $cuda $splash"
  entry nano "linux-nano: desktop on nouveau, no CUDA (fallback)" nano-j202-cuda \
    "$args console=tty0 $tail nouveau.config=NvClkMode=0x07 modprobe.blacklist=nvgpu cma=256M"
  entry nano-oc "linux-nano: CPU 1785 MHz, GPU 1152 MHz (beyond NVIDIA's clocks)" nano-j202-cuda "$args $tail $cuda $oc $splash"
} > "$out/extlinux.conf"
if [[ -n $DEFAULT ]]; then
  grep -qx "LABEL $DEFAULT" "$out/extlinux.conf" || die "no entry $DEFAULT"
  sed -i "s/^DEFAULT .*/DEFAULT $DEFAULT/" "$out/extlinux.conf"
fi
d=$(awk '$1 == "DEFAULT" { print $2; exit }' "$out/extlinux.conf")
grep -qx "LABEL $d" "$out/extlinux.conf" || die "the default entry $d would not exist: --default nano-cuda"
grep -E '^(DEFAULT|LABEL)' "$out/extlinux.conf"

theme=$here/omarchy/nano/files/usr/share/plymouth/themes/jetson-nano
picture=$here/omarchy/nano/bootlogo-j202.png
if ((LOGO)); then
  say "first picture"
  for c in magick lz4 python3; do command -v $c >/dev/null || die "--logo needs $c (pacman -S imagemagick lz4 python)"; done
  python3 "$here/omarchy/nano/bootlogo.py" build /dev/disk/by-partlabel/BMP "$picture" "$out/bootlogo.blob"
fi

if ((DRY)); then
  say "dry run: everything is in $out, nothing was changed"
  exit 0
fi

save=/root/boot-before-$(date +%Y%m%d-%H%M%S)
say "saving what is replaced in $save"
mkdir -p "$save"
for f in Image-nano initramfs-linux-nano.img j202 extlinux; do
  if [[ -e $BOOT/$f ]]; then cp -a "$BOOT/$f" "$save/"; fi
done
if [[ -d /usr/share/plymouth/themes/jetson-nano ]]; then cp -a /usr/share/plymouth/themes/jetson-nano "$save/plymouth-jetson-nano"; fi

say "installing"
mkdir -p "$BOOT/j202"
# each file is complete before it takes its name
put() { install -m "$3" "$1" "$2.new" && sync "$2.new" && mv "$2.new" "$2"; }
for f in nano-j202 nano-j202-cuda; do put "$out/j202/$f.dtb" "$BOOT/j202/$f.dtb" 644; done
if [[ -f $out/Image-nano ]]; then
  put "$out/Image-nano" "$BOOT/Image-nano" 644
  put "$out/initramfs-linux-nano.img" "$BOOT/initramfs-linux-nano.img" 600
fi
if [[ -f $theme/splash.png ]]; then
  install -Dm644 -t /usr/share/plymouth/themes/jetson-nano "$theme"/*
  if ! grep -qs '^Theme=' /etc/plymouth/plymouthd.conf; then
    install -d /etc/plymouth && printf '[Daemon]\nTheme=jetson-nano\n' > /etc/plymouth/plymouthd.conf
  fi
fi
put "$out/extlinux.conf" "$EXT" 644
if ((LOGO)); then python3 "$here/omarchy/nano/bootlogo.py" install "$out/bootlogo.blob"; fi
sync
say "done. Back: copy $save to /boot$( ((LOGO)) && echo ', bootlogo.py restore /root/BMP-*.img for the first picture')"

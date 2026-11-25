#!/bin/bash
# reComputer J1020 v2: move the root filesystem from the eMMC to an NVMe SSD. /boot stays on the
# eMMC, where L4T U-Boot reads it. THE SSD IS ERASED; the eMMC root is left as it is (fallback).
#
# As root on the board, booted from the eMMC (entry "mainline", see flash-j202-emmc.sh):
#   bash scripts/j202-root-to-nvme.sh /dev/nvme0n1           shows what it would do
#   bash scripts/j202-root-to-nvme.sh /dev/nvme0n1 --erase   does it
# It makes one ext4 partition (label NANO_ROOT), copies the root, writes the new fstab and adds the
# boot entries "nvme" (same kernel as the default entry) and "nano-plain" (linux-nano, which
# scripts/build-nano-stack.sh starts from). DEFAULT becomes nvme; pick "mainline" in the U-Boot menu
# (serial console) to go back.
#
# For tests: SRC (default /), EXTLINUX, BOOT_PART and ALLOW_LOOP=1 (a loop device as the target).
set -euo pipefail

disk=${1:-} mode=${2:-}
SRC=${SRC:-/}
EXTLINUX=${EXTLINUX:-/boot/extlinux/extlinux.conf}
BOOT_PART=${BOOT_PART:-/dev/mmcblk0p1}
die() { echo "error: $*" >&2; exit 1; }
say() { printf '>> %s\n' "$*"; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ -n $disk && ( -z $mode || $mode == --erase ) ]] || die "usage: $0 /dev/nvme0n1 [--erase]"
[[ $(lsblk -dno TYPE "$disk" 2>/dev/null) == disk || ( ${ALLOW_LOOP:-} == 1 && $disk == /dev/loop* ) ]] \
  || die "$disk is not a whole disk"
[[ $disk == /dev/nvme* || ${ALLOW_LOOP:-} == 1 ]] || die "$disk is not an NVMe disk"
if lsblk -nro MOUNTPOINTS "$disk" | grep -q .; then die "$disk is in use (mounted)"; fi
if [[ $SRC == / ]]; then
  [[ $(findmnt -no SOURCE /) == "$BOOT_PART" ]] || die "the root is not on $BOOT_PART: boot the eMMC entry first"
fi
[[ -f $EXTLINUX ]] || die "no $EXTLINUX"
for c in sfdisk mkfs.ext4 rsync blkid; do command -v $c >/dev/null || die "needs $c (pacman -S rsync util-linux e2fsprogs)"; done

label=$(awk '$1 == "DEFAULT" { print $2; exit }' "$EXTLINUX")
field() { awk -v l="$label" -v k="$1" '$1 == "LABEL" { p = ($2 == l) } p && $1 == k { sub(/^[[:space:]]*[A-Z]+[[:space:]]+/, ""); print; exit }' "$EXTLINUX"; }
linux=$(field LINUX) initrd=$(field INITRD) fdt=$(field FDT) append=$(field APPEND)
[[ -n $linux && -n $initrd && -n $fdt && -n $append ]] || die "the default entry ($label) of $EXTLINUX is incomplete"

used=$(du -xsm --exclude=swapfile "$SRC" 2>/dev/null | cut -f1)
size=$(( $(blockdev --getsize64 "$disk") / 1048576 ))
say "disk: $disk $(lsblk -dno MODEL,SIZE "$disk" | xargs)"
lsblk -o NAME,SIZE,FSTYPE,LABEL "$disk"
say "to copy: ${used} MB from $SRC; boot entry to reuse: $label"
(( size > used * 12 / 10 )) || die "$disk is too small (${size} MB)"
if [[ $mode != --erase ]]; then
  say "nothing done. Run again with --erase to ERASE $disk and copy the root to it."
  exit 0
fi

say "partition and format"
wipefs -qa "$disk"
sfdisk -q "$disk" <<'EOF'
label: gpt
type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="nano-root"
EOF
udevadm settle
part=${disk}p1
[[ -b $part ]] || die "$part did not appear"
mkfs.ext4 -q -F -L NANO_ROOT "$part"
partuuid=$(blkid -p -s PART_ENTRY_UUID -o value "$part")
[[ -n $partuuid ]] || die "no PARTUUID for $part"

mnt=$(mktemp -d)
trap 'umount "$mnt" 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true' EXIT
mount "$part" "$mnt"
say "copy"
rsync -aHAXx --numeric-ids --exclude=/boot/* --exclude=/swapfile --exclude=/lost+found "${SRC%/}/" "$mnt/"
mkdir -p "$mnt/boot" "$mnt/mnt/emmc" "$mnt"/{dev,proc,run,sys,tmp}
chmod 1777 "$mnt/tmp"
[[ -f $mnt/etc/fstab ]] && cp -p "$mnt/etc/fstab" "$mnt/etc/fstab.emmc"
mkdir -p "$mnt/etc"
cat > "$mnt/etc/fstab" <<EOF
# root on the NVMe SSD; the eMMC keeps the boot files U-Boot reads (extlinux, kernels, dtbs)
LABEL=NANO_ROOT  /           ext4  rw,relatime  0 1
$BOOT_PART   /mnt/emmc   ext4  rw,relatime,nofail  0 2
/mnt/emmc/boot   /boot       none  bind,x-systemd.requires-mounts-for=/mnt/emmc,nofail  0 0
EOF
sync
umount "$mnt"
e2fsck -fn "$part" >/dev/null || die "the new filesystem has errors; $EXTLINUX is unchanged"

say "boot entries"
cp -p "$EXTLINUX" "$EXTLINUX.pre-nvme"
# the arguments of the default entry, without its root, once each
rest=$(tr -s ' ' '\n' <<<"$append" | awk 'NF && !/^(root=|rootfstype=|rootdelay=|rw$|rootwait$)/ && !seen[$0]++' | tr '\n' ' ')
root="root=PARTUUID=$partuuid rw rootwait rootfstype=ext4"
new=$(mktemp)
awk '$1 == "LABEL" { p = ($2 == "nvme" || $2 == "nano-plain") } !p' "$EXTLINUX" | sed 's/^DEFAULT .*/DEFAULT nvme/' > "$new"
{
  printf '\nLABEL nvme\n      MENU LABEL Arch Linux ARM, root on the NVMe SSD\n'
  printf '      LINUX %s\n      INITRD %s\n      FDT %s\n' "$linux" "$initrd" "$fdt"
  printf '      APPEND %s rootdelay=30 %s\n' "$root" "${rest% }"
  printf '\nLABEL nano-plain\n      MENU LABEL linux-nano, root on the NVMe SSD\n'
  printf '      LINUX /boot/Image-nano\n      INITRD /boot/initramfs-linux-nano.img\n      FDT /boot/j202/nano-j202.dtb\n'
  printf '      APPEND %s console=ttyS0,115200n8 console=tty0 loglevel=7 fbcon=map:0 net.ifnames=0 clk_ignore_unused pd_ignore_unused panic=10 cma=256M\n' "$root"
} >> "$new"
grep -q '^DEFAULT nvme$' "$new" || die "no DEFAULT line in $EXTLINUX; it is unchanged"
cat "$new" > "$EXTLINUX"
rm -f "$new"
sync
grep -E '^(DEFAULT|LABEL)' "$EXTLINUX"
say "done: reboot. Back to the eMMC: entry $label in the U-Boot menu, or $EXTLINUX.pre-nvme"

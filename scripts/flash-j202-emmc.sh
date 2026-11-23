#!/bin/bash
# Flash a Seeed reComputer J1020 v2 (Jetson Nano P3448-0002 eMMC on a J202
# carrier) with NVIDIA's L4T R32.7.6 bootloader and an Arch Linux ARM root
# filesystem that boots the mainline kernel with the J202 device tree.
#
# Runs on an x86_64 Linux host or WSL2 (Ubuntu). THE WHOLE eMMC IS ERASED.
#
# What lands on the eMMC:
#   boot0/boot1 + bootloader partitions : NVIDIA L4T R32.7.6 (cboot, U-Boot), stock
#   APP (mmcblk0p1, ext4)               : Arch Linux ARM aarch64 rootfs
#     /boot/mainline/Image              : ALARM's linux-aarch64 kernel (mainline)
#     /boot/mainline/j202.dtb           : images/mainline-kernel/j202/tegra210-p3448-0002-j202.dtb
#     /boot/mainline/initramfs.img      : minimal initramfs (i2c-tegra, sdhci-tegra)
#     /boot/extlinux/extlinux.conf      : boots the three files above
# flash.sh also drops NVIDIA's own 4.9 Image/initrd/DTB into /boot; they are unused.
#
# A small initramfs is required: ALARM builds i2c-tegra as a module, and the
# MAX77620 PMIC behind it powers the eMMC (first boot without one hung at
# "Waiting for root device"). The J202 DTB pins mmc0 to the eMMC, so
# root=/dev/mmcblk0p1 is stable.
#
# Before running:
#   1. Board off, jumper FC REC to GND on the J202, power on (recovery mode).
#   2. WSL2 only, in an ADMIN PowerShell on Windows:
#        usbipd list                                   (find the 0955:7f21 "APX" device)
#        usbipd bind --busid <BUSID>
#        usbipd attach --wsl --busid <BUSID> --auto-attach
#      --auto-attach matters: the Nano re-enumerates during the flash.
#   3. In WSL: sudo bash scripts/flash-j202-emmc.sh
#
# Options:
#   --prepare-only   build the Linux_for_Tegra tree and rootfs, do not flash
#   --flash-only     flash an already prepared tree
# Environment: WORK (default /var/tmp/j202-flash), LOG (default: none)
set -euo pipefail

L4T_URL=https://developer.download.nvidia.com/embedded/L4T/r32_Release_v7.6/T210/Jetson-210_Linux_R32.7.6_aarch64.tbz2
ALARM_URL=http://os.archlinuxarm.org/os/ArchLinuxARM-aarch64-latest.tar.gz
BOARD=jetson-nano-emmc
WORK=${WORK:-/var/tmp/j202-flash}
REPO=$(cd "$(dirname "$0")/.." && pwd)
DTB_SRC=$REPO/images/mainline-kernel/j202/tegra210-p3448-0002-j202.dtb

PREPARE=1 FLASH=1 APP_ONLY=0
case ${1:-} in
  --prepare-only) FLASH=0 ;;
  --flash-only) PREPARE=0 ;;
  --app-only) APP_ONLY=1 ;;   # rebuild the rootfs, rewrite only mmcblk0p1 (bootloader untouched)
  "") ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac

if [[ -n ${LOG:-} ]]; then exec > >(tee -a "$LOG") 2>&1; fi
say() { printf '\n>> [%(%T)T] %s\n' -1 "$*"; }
die() { printf '\nERREUR: %s\n' "$*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "lancer avec sudo"
[[ $(uname -m) == x86_64 ]] || die "hôte x86_64 requis (les outils de flash NVIDIA sont x86_64)"
[[ -f $DTB_SRC ]] || die "DTB introuvable : $DTB_SRC"

LDK=$WORK/Linux_for_Tegra
CACHE=$WORK/cache

need_pkgs() {
  local missing=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if ((${#missing[@]})); then
    say "installation des paquets hôte : ${missing[*]}"
    apt-get update -qq && apt-get install -y -qq "${missing[@]}"
  fi
}

# The PMIC (MAX77620) that powers the eMMC sits behind i2c-tegra, which ALARM
# builds as a module: without an initramfs the eMMC never probes. Build a small
# one (no autodetect, so it does not depend on the build host) in an aarch64
# chroot through qemu-user-static. Kept small because L4T U-Boot places the
# initrd only ~14 MiB away from the kernel.
build_initramfs() {
  local R=$1 kver=$2
  say "génération de l'initramfs J202 (chroot aarch64 via qemu)"
  [[ -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ]] || update-binfmts --enable qemu-aarch64 || true
  [[ -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ]] || die "émulation aarch64 (binfmt qemu-aarch64) indisponible"
  cat > "$R/etc/mkinitcpio-j202.conf" <<'EOF'
# reComputer J1020 v2: i2c-tegra reaches the MAX77620 PMIC that powers the eMMC.
MODULES=(i2c-tegra sdhci-tegra)
BINARIES=()
FILES=()
# No block/filesystems/fsck hooks: without autodetect they pull every storage
# driver (20 MiB), and ext4, sdhci-tegra, pcie-tegra and nvme are built in.
HOOKS=(base udev modconf)
COMPRESSION="zstd"
COMPRESSION_OPTIONS=(-19)
EOF
  local m
  for m in proc sys dev; do mount --bind "/$m" "$R/$m"; done
  # shellcheck disable=SC2064
  trap "umount -l '$R/proc' '$R/sys' '$R/dev' 2>/dev/null || true" EXIT
  chroot "$R" /usr/bin/mkinitcpio -c /etc/mkinitcpio-j202.conf -k "$kver" -g /boot/mainline/initramfs.img
  umount -l "$R/proc" "$R/sys" "$R/dev"; trap - EXIT
  local size
  size=$(stat -c %s "$R/boot/mainline/initramfs.img")
  say "initramfs : $((size / 1024)) Kio"
  ((size < 12 * 1024 * 1024)) || die "initramfs trop gros pour U-Boot ($size octets)"
}

prepare() {
  need_pkgs qemu-user-static binfmt-support curl bzip2 libarchive-tools python3 python-is-python3 binutils cpio \
            libxml2-utils usbutils device-tree-compiler xxd
  mkdir -p "$CACHE"

  say "téléchargement du BSP L4T R32.7.6"
  curl -fL --retry 3 -C - -o "$CACHE/l4t.tbz2" "$L4T_URL"
  say "téléchargement d'Arch Linux ARM (aarch64)"
  curl -fL --retry 3 -C - -o "$CACHE/alarm.tar.gz" "$ALARM_URL"
  curl -fsSL -o "$CACHE/alarm.tar.gz.md5" "$ALARM_URL.md5"
  say "vérification md5 d'Arch Linux ARM"
  (cd "$CACHE" && awk '{print $1"  alarm.tar.gz"}' alarm.tar.gz.md5 | md5sum -c -) \
    || die "md5 incorrect, supprimer $CACHE/alarm.tar.gz et relancer"

  say "extraction du BSP"
  rm -rf "$LDK"
  tar -xjf "$CACHE/l4t.tbz2" -C "$WORK"
  [[ -f $LDK/flash.sh && -f $LDK/$BOARD.conf ]] || die "BSP incomplet"

  say "extraction du rootfs Arch dans Linux_for_Tegra/rootfs (environ 1 min)"
  rm -rf "$LDK/rootfs" && mkdir -p "$LDK/rootfs"
  bsdtar -xpf "$CACHE/alarm.tar.gz" -C "$LDK/rootfs"
  local R=$LDK/rootfs
  [[ -f $R/boot/Image ]] || die "pas de /boot/Image dans l'archive Arch"
  local kver
  kver=$(ls "$R/usr/lib/modules" | head -n1)
  say "kernel Arch trouvé : $kver"

  say "installation du kernel mainline et du DTB J202 dans /boot/mainline"
  install -d "$R/boot/mainline"
  install -m 0644 "$R/boot/Image" "$R/boot/mainline/Image"
  install -m 0644 "$DTB_SRC" "$R/boot/mainline/j202.dtb"
  fdtget "$R/boot/mainline/j202.dtb" /mmc@700b0600 status | grep -qx okay \
    || die "le DTB n'active pas l'eMMC"

  build_initramfs "$R" "$kver"

  say "écriture de /boot/extlinux/extlinux.conf"
  install -d "$R/boot/extlinux"
  cat > "$R/boot/extlinux/extlinux.conf" <<EOF
TIMEOUT 30
DEFAULT mainline

MENU TITLE reComputer J1020 v2 boot

LABEL mainline
      MENU LABEL Arch Linux ARM, mainline $kver, J202 DTB
      LINUX /boot/mainline/Image
      INITRD /boot/mainline/initramfs.img
      FDT /boot/mainline/j202.dtb
      APPEND root=/dev/mmcblk0p1 rw rootwait rootfstype=ext4 console=ttyS0,115200n8 console=tty0 loglevel=7
EOF

  say "configuration de base"
  printf '/dev/mmcblk0p1  /  ext4  rw,relatime  0 1\n' > "$R/etc/fstab"
  printf 'recomputer\n' > "$R/etc/hostname"
  # ALARM ships sshd and systemd-networkd (DHCP on en*/eth*); make sure both start.
  for unit in sshd.service systemd-networkd.service systemd-resolved.service; do
    if [[ -f $R/usr/lib/systemd/system/$unit ]]; then
      ln -sf "/usr/lib/systemd/system/$unit" "$R/etc/systemd/system/multi-user.target.wants/$unit"
    fi
  done
  if ! ls "$R"/usr/lib/firmware/nvidia/tegra210/xusb.bin* >/dev/null 2>&1; then
    say "ATTENTION : firmware nvidia/tegra210/xusb.bin absent : USB 3 inactif au 1er boot (Ethernet OK)"
  fi
  du -sh "$R" | awk '{print ">> taille du rootfs : "$1}'
}

flash() {
  [[ -f $LDK/flash.sh && -f $LDK/rootfs/boot/mainline/j202.dtb ]] \
    || die "arbre non préparé, lancer sans --flash-only"
  say "recherche du Jetson en mode recovery (0955:7f21)"
  if ! lsusb -d 0955:7f21 >/dev/null 2>&1; then
    die "Jetson non vu en USB. Mettre FC REC à GND, allumer, puis sous Windows (admin) :
  usbipd list
  usbipd bind --busid <BUSID>
  usbipd attach --wsl --busid <BUSID> --auto-attach"
  fi
  lsusb -d 0955:7f21
  say "FLASH de l'eMMC entière ($BOARD, rootfs sur mmcblk0p1) : environ 10 à 20 min"
  cd "$LDK"
  if ((APP_ONLY)); then
    ./flash.sh -k APP "$BOARD" mmcblk0p1
  else
    ./flash.sh "$BOARD" mmcblk0p1
  fi
  say "terminé. Retirer le cavalier FC REC, redémarrer, et suivre la console série (115200 8N1)."
  cat <<EOF
Premier démarrage :
  - identifiants Arch par défaut : root/root et alarm/alarm (à changer tout de suite : passwd)
  - SSH : ssh alarm@recomputer.local, ou l'IP donnée par ton routeur
  - puis : pacman-key --init && pacman-key --populate archlinuxarm && pacman -Syu
EOF
}

((PREPARE)) && prepare
((FLASH)) && flash
exit 0

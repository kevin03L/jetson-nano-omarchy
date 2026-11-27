#!/bin/bash
# reComputer J1020 v2 (J202): build j202-rootfs-VERSION.img.zst, the system image of a release — the
# one the Windows installer writes to the eMMC's APP partition.
#
# Run on an aarch64 Arch Linux ARM host (the board itself, or an ARM container), as root, from a
# checkout, with the release's packages and j202-boot archive already built:
#   bash scripts/j202-make-rootfs.sh VERSION PKGDIR BOOTARCHIVE [OUT_DIR]
#     VERSION       e.g. v1.3
#     PKGDIR        a directory holding the release's *.pkg.tar.zst (linux-nano, nvgpu-nano,
#                   jetson-nano-board, vaapi-tegra, mesa-tegra, and the Omarchy meta-package)
#     BOOTARCHIVE   the release's j202-boot-VERSION.tar.zst (scripts/j202-make-boot-archive.sh)
#     OUT_DIR       where to write the image (default: .)
#
# What goes in, and what does NOT:
#   - A fresh Arch Linux ARM aarch64 rootfs with the release's packages installed.
#   - j202-boot installed (kernel, device trees, boot menu, loading screen).
#   - A first-boot service that, once, on the real board: creates the user from /boot/j202-firstboot.conf
#     (written by the Windows installer), grows the root partition, runs j202-install-boot.sh to take
#     this board's memory timings, installs the NVDEC firmware from the L4T archive, then deletes
#     itself and the config.
#   - NO user, NO password, NO SSH key, NO NVDEC firmware (NVIDIA's licence): those arrive at install
#     or first boot. The image is the same for everyone and carries no secret.
#
# This script has NOT been run end to end yet: it needs an aarch64 host and the built release. Treat
# it as the recipe to produce the release image, to verify on the board.
set -euo pipefail

ALARM_URL=http://os.archlinuxarm.org/os/ArchLinuxARM-aarch64-latest.tar.gz
here=$(cd "$(dirname "$0")/.." && pwd)
version=${1:?usage: $0 VERSION PKGDIR BOOTARCHIVE [OUT_DIR]}
pkgdir=$(realpath "${2:?PKGDIR}")
bootarchive=$(realpath "${3:?BOOTARCHIVE}")
dest=$(realpath -m "${4:-.}")
die() { echo "error: $*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ $(uname -m) == aarch64 ]] || die "build on an aarch64 host (the board or an ARM container)"
[[ -d $pkgdir ]] || die "no package directory $pkgdir"
[[ -f $bootarchive ]] || die "no boot archive $bootarchive"
for c in pacstrap sfdisk mkfs.ext4 zstd bsdtar; do
  command -v "$c" >/dev/null || die "needs $c (pacman -S arch-install-scripts e2fsprogs zstd libarchive)"
done
pkgs=("$pkgdir"/*.pkg.tar.zst)
[[ -e ${pkgs[0]} ]] || die "no *.pkg.tar.zst in $pkgdir"

tmp=$(mktemp -d /var/tmp/j202-rootfs.XXXXXX)
root=$tmp/root
mkdir -p "$root"
cleanup() { umount -R "$root" 2>/dev/null || true; rm -rf "$tmp"; }
trap cleanup EXIT

echo ">> base Arch Linux ARM aarch64"
curl -fL --retry 3 -o "$tmp/alarm.tar.gz" "$ALARM_URL"
bsdtar -xpf "$tmp/alarm.tar.gz" -C "$root"

# the real package database, so the release's packages resolve their dependencies
echo ">> packages of the release"
mount --bind "$root" "$root"            # pacstrap wants a mountpoint
cp "${pkgs[@]}" "$root/var/cache/pacman/pkg/" 2>/dev/null || true
pacman -r "$root" -Sy --noconfirm --needed base "${pkgs[@]}" \
  || die "the release's packages did not install (check PKGDIR holds all dependencies)"

echo ">> boot files of the release"
bsdtar -xpf "$bootarchive" -C "$tmp"
install -Dm755 "$here/scripts/j202-install-boot.sh" "$root/usr/local/lib/j202/j202-install-boot.sh" 2>/dev/null || true
install -Dm755 "$here/scripts/j202-root-to-nvme.sh" "$root/usr/local/lib/j202/j202-root-to-nvme.sh" 2>/dev/null || true
cp -r "$tmp/j202-boot" "$root/usr/local/lib/j202/boot-archive"

echo ">> first-boot service (creates the user, grows the partition, sets the boot up)"
install -d "$root/usr/local/lib/j202"
cat > "$root/usr/local/lib/j202/firstboot.sh" <<'FB'
#!/bin/bash
# Runs once, on the real board. Reads /boot/j202-firstboot.conf (written by the installer), then
# removes itself. Never bakes a secret into the image.
set -euo pipefail
conf=/boot/j202-firstboot.conf
log=/var/log/j202-firstboot.log
exec >>"$log" 2>&1
echo "=== j202 first boot $(date) ==="

# 1. grow the root partition and filesystem to the whole eMMC
root_part=$(findmnt -no SOURCE /)
disk=/dev/$(lsblk -no PKNAME "$root_part")
partnum=$(echo "$root_part" | grep -o '[0-9]*$')
if command -v growpart >/dev/null 2>&1; then
  growpart "$disk" "$partnum" || true
fi
resize2fs "$root_part" || true

# 2. the account, from the config the installer wrote (skip if still the placeholder)
move_nvme=0
if [[ -f $conf ]]; then
  if ! grep -q 'J202_FIRSTBOOT_PLACEHOLDER_V1' "$conf"; then
    # shellcheck disable=SC1090
    source "$conf"
    move_nvme=${J202_NVME:-0}
    if [[ -n ${J202_USER:-} ]]; then
      useradd -m -G wheel,video,audio,input -s /bin/bash "$J202_USER" || true
      if [[ -n ${J202_PASS_HASH:-} ]]; then
        usermod -p "$J202_PASS_HASH" "$J202_USER" || true
      fi
      [[ -n ${J202_HOST:-} ]] && echo "$J202_HOST" > /etc/hostname
      if [[ -n ${J202_TZ:-} && -e /usr/share/zoneinfo/$J202_TZ ]]; then
        ln -sf "/usr/share/zoneinfo/$J202_TZ" /etc/localtime
      fi
      if [[ -n ${J202_LOCALE:-} ]]; then
        sed -i -E "s|^#\s*(${J202_LOCALE}[[:space:]])|\1|" /etc/locale.gen || true
        locale-gen || true
        printf 'LANG=%s\n' "$J202_LOCALE" > /etc/locale.conf
      fi
      if [[ -n ${J202_KEYMAP:-} || -n ${J202_XKBLAYOUT:-} ]]; then
        {
          [[ -n ${J202_KEYMAP:-} ]] && printf 'KEYMAP=%s\n' "$J202_KEYMAP"
          [[ -n ${J202_XKBLAYOUT:-} ]] && printf 'XKBLAYOUT=%s\n' "$J202_XKBLAYOUT"
          [[ -n ${J202_XKBVARIANT:-} ]] && printf 'XKBVARIANT=%s\n' "$J202_XKBVARIANT"
        } > /etc/vconsole.conf
        localectl set-x11-keymap "${J202_XKBLAYOUT:-us}" "" "${J202_XKBVARIANT:-}" || true
      fi
      if [[ -n ${J202_XKBLAYOUT:-} && -d /home/$J202_USER/.config/hypr ]]; then
        printf '\nhl.config({ input = { kb_layout = "%s", kb_variant = "%s" } })\n' \
          "$J202_XKBLAYOUT" "${J202_XKBVARIANT:-}" >> "/home/$J202_USER/.config/hypr/input.lua"
        chown "$J202_USER:$J202_USER" "/home/$J202_USER/.config/hypr/input.lua"
      fi
      passwd -l root || true
      id alarm >/dev/null 2>&1 && passwd -l alarm || true
    fi
  fi
  shred -u "$conf" 2>/dev/null || rm -f "$conf"
fi

# 3. boot files tuned to this board (memory timings from cboot), then the NVDEC firmware from L4T
if [[ -d /usr/local/lib/j202/boot-archive ]]; then
  if command -v magick >/dev/null 2>&1 && command -v lz4 >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    bash /usr/local/lib/j202/boot-archive/scripts/j202-install-boot.sh --default nano-cuda --logo || true
  else
    bash /usr/local/lib/j202/boot-archive/scripts/j202-install-boot.sh --default nano-cuda || true
  fi
fi
l4t=$(ls /boot/Jetson-210_Linux_R*_aarch64.tbz2 2>/dev/null | head -n1 || true)
if [[ -n $l4t ]]; then
  tmp=$(mktemp -d)
  bsdtar -xpf "$l4t" -C "$tmp" Linux_for_Tegra/nv_tegra/nvidia_drivers.tbz2 2>/dev/null || true
  f=$(find "$tmp" -name 'nvdec*.bin' | head -n1 || true)
  [[ -n $f ]] && install -Dm644 "$f" /usr/lib/firmware/nvidia/tegra210/nvdec.bin || true
  rm -rf "$tmp" "$l4t"
fi

systemctl disable j202-firstboot.service || true
rm -f /etc/systemd/system/multi-user.target.wants/j202-firstboot.service

if [[ $move_nvme == 1 ]]; then
  if [[ -b /dev/nvme0n1 && -x /usr/local/lib/j202/j202-root-to-nvme.sh ]]; then
    echo "=== moving rootfs to /dev/nvme0n1 ==="
    if bash /usr/local/lib/j202/j202-root-to-nvme.sh /dev/nvme0n1 --erase; then
      sync
      echo "=== NVMe migration complete, rebooting ==="
      systemctl reboot --no-block || reboot
      exit 0
    else
      echo "WARNING: NVMe migration failed, staying on eMMC"
    fi
  else
    echo "WARNING: /dev/nvme0n1 not found, staying on eMMC"
  fi
fi

echo "=== first boot done ==="
FB
chmod 755 "$root/usr/local/lib/j202/firstboot.sh"
cat > "$root/etc/systemd/system/j202-firstboot.service" <<'UNIT'
[Unit]
Description=J202 first boot setup (account, partition, boot, NVDEC firmware)
ConditionPathExists=/usr/local/lib/j202/firstboot.sh
After=local-fs.target
Before=getty.target

[Service]
Type=oneshot
ExecStart=/usr/local/lib/j202/firstboot.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
ln -sf /etc/systemd/system/j202-firstboot.service \
  "$root/etc/systemd/system/multi-user.target.wants/j202-firstboot.service"

printf '/dev/mmcblk0p1  /  ext4  rw,relatime  0 1\n' > "$root/etc/fstab"
printf 'jetson-nano\n' > "$root/etc/hostname"

# Fixed 4096-byte placeholder in /boot so the Windows installer can patch it in place inside the
# streamed ext4 image without mounting or modifying ext4 metadata.
python3 - "$root/boot/j202-firstboot.conf" <<'PY'
import sys
hdr = b"# J202_FIRSTBOOT_PLACEHOLDER_V1\n"
data = hdr + b"#" * (4096 - len(hdr) - 1) + b"\n"
assert len(data) == 4096
open(sys.argv[1], "wb").write(data)
PY

echo ">> packing the ext4 image"
umount -R "$root" 2>/dev/null || true
# size the image to the content plus room for first boot; the partition is grown on the board anyway
used_kib=$(du -sk --exclude=var/cache/pacman/pkg "$root" | cut -f1)
img_mib=$(( used_kib / 1024 + 900 ))
img=$tmp/rootfs.img
truncate -s "${img_mib}M" "$img"
mkfs.ext4 -q -L NANO_ROOT -d "$root" "$img"
mkdir -p "$dest"
zstd -q -19 -T0 -f "$img" -o "$dest/j202-rootfs-$version.img.zst"
ls -l "$dest/j202-rootfs-$version.img.zst"
echo ">> done. Verify it on the board before a release."

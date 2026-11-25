#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib-extlinux.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cat > "$TMP_DIR/a.conf" <<'EOF'
LABEL primary
      LINUX /boot/Image
      INITRD /boot/initrd
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} quiet root=/dev/mmcblk0p1 rw rootwait video=HDMI-A-1:e
EOF

cat > "$TMP_DIR/expected.conf" <<'EOF'
LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 panic=10 hung_task_panic=1 softlockup_panic=1 reserve_mem=1M:4096:oops ramoops.mem_name=oops ramoops.record_size=0x40000 clk_ignore_unused pd_ignore_unused
EOF

# already provisioned with every token: unchanged
cat > "$TMP_DIR/b.conf" <<'EOF'
LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 panic=10 hung_task_panic=1 softlockup_panic=1 reserve_mem=1M:4096:oops ramoops.mem_name=oops ramoops.record_size=0x40000 clk_ignore_unused pd_ignore_unused
EOF

# provisioned before the panic/ramoops tokens: only those are appended
cat > "$TMP_DIR/c.conf" <<'EOF'
LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 panic=10 clk_ignore_unused pd_ignore_unused
EOF
cat > "$TMP_DIR/expected-c.conf" <<'EOF'
LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 panic=10 clk_ignore_unused pd_ignore_unused hung_task_panic=1 softlockup_panic=1 reserve_mem=1M:4096:oops ramoops.mem_name=oops ramoops.record_size=0x40000
EOF

edit_extlinux "$TMP_DIR/a.conf"
if diff -u "$TMP_DIR/expected.conf" "$TMP_DIR/a.conf"; then
  printf 'PASS fixture A\n'
else
  printf 'FAIL fixture A\n' >&2
  exit 1
fi

edit_extlinux "$TMP_DIR/b.conf"
if diff -u "$TMP_DIR/expected.conf" "$TMP_DIR/b.conf"; then
  printf 'PASS fixture B\n'
else
  printf 'FAIL fixture B\n' >&2
  exit 1
fi

edit_extlinux "$TMP_DIR/c.conf"
if diff -u "$TMP_DIR/expected-c.conf" "$TMP_DIR/c.conf"; then
  printf 'PASS fixture C\n'
else
  printf 'FAIL fixture C\n' >&2
  exit 1
fi

# D: add_nano_entries on a provisioned stock file adds both entries and the default, once
cat > "$TMP_DIR/d.conf" <<'EOF'
TIMEOUT 30
DEFAULT primary

LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7
EOF
cat > "$TMP_DIR/expected-d.conf" <<'EOF'
TIMEOUT 30
DEFAULT nano-cuda

LABEL primary
      LINUX /boot/Image
      INITRD /boot/initramfs-linux.img
      FDT /boot/tegra210-p3448-0002-p3449-0000-b00.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7

LABEL nano-cuda
      MENU LABEL linux-nano: desktop and CUDA on nvgpu
      LINUX /boot/Image-nano
      INITRD /boot/initramfs-linux-nano.img
      FDT /boot/nano.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 cma=256M modprobe.blacklist=nouveau nano.mode=cuda

LABEL nano
      MENU LABEL linux-nano: desktop on nouveau, no CUDA (fallback)
      LINUX /boot/Image-nano
      INITRD /boot/initramfs-linux-nano.img
      FDT /boot/nano.dtb
      APPEND ${cbootargs} root=LABEL=ALARM rw rootwait video=HDMI-A-1:1280x720@60e loglevel=7 cma=256M nouveau.config=NvClkMode=0x07 modprobe.blacklist=nvgpu
EOF
add_nano_entries "$TMP_DIR/d.conf" nano.dtb
add_nano_entries "$TMP_DIR/d.conf" nano.dtb
if diff -u "$TMP_DIR/expected-d.conf" "$TMP_DIR/d.conf"; then
  printf 'PASS fixture D (entries added once)\n'
else
  printf 'FAIL fixture D\n' >&2
  exit 1
fi

# D2: entries made before cma=256M get it once; another cma= and the other entries are kept
sed -e '/nano.mode=cuda/ s/ cma=256M//' -e '/NvClkMode/ s/cma=256M/cma=384M/' "$TMP_DIR/expected-d.conf" >"$TMP_DIR/d2.conf"
sed -e '/nano.mode=cuda/ { s/ cma=256M//; s/$/ cma=256M/ }' -e '/NvClkMode/ s/cma=256M/cma=384M/' "$TMP_DIR/expected-d.conf" >"$TMP_DIR/expected-d2.conf"
add_nano_entries "$TMP_DIR/d2.conf" nano.dtb
add_nano_entries "$TMP_DIR/d2.conf" nano.dtb
if diff -u "$TMP_DIR/expected-d2.conf" "$TMP_DIR/d2.conf"; then
  printf 'PASS fixture D2 (cma added to existing entries)\n'
else
  printf 'FAIL fixture D2\n' >&2
  exit 1
fi

# E: edit_extlinux on a file with the linux-nano entries keeps their initramfs
cp "$TMP_DIR/expected-d.conf" "$TMP_DIR/e.conf"
edit_extlinux "$TMP_DIR/e.conf"
if [[ $(grep -c 'INITRD /boot/initramfs-linux-nano.img' "$TMP_DIR/e.conf") == 2 ]] \
  && [[ $(grep -c 'INITRD /boot/initramfs-linux.img' "$TMP_DIR/e.conf") == 1 ]]; then
  printf 'PASS fixture E (nano initramfs kept)\n'
else
  printf 'FAIL fixture E\n' >&2
  grep INITRD "$TMP_DIR/e.conf" >&2
  exit 1
fi

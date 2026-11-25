#!/bin/bash
# reComputer J1020 v2: build the linux-nano device tree with the EMC tables cboot trained.
#
# cboot trains the DRAM on every boot and leaves its own kernel device tree in RAM at
# 0x83100000. The J202 DTB reserves that region (no-map), so it can be read through /dev/mem.
# This copies its trained EMC tables for our RAM code into the linux-nano J202 DTB, so the
# tegra210-emc driver (patch 0101) can run the memory at 1600 MHz instead of 204 MHz.
#
# Usage (on the board, as root, booted with a J202 DTB that has the reserved region):
#   make-j202-nano-dtb.sh [IN_DTB] [OUT_DTB]
# Defaults: the packaged linux-nano J202 DTB in, /boot/j202/nano-j202.dtb out.
# Same idea as the "static trained EMC table" of the original project: the tables are one boot's
# training; run it again after a bootloader update.
set -euo pipefail

in=${1:-/boot/dtbs-nano/nvidia/tegra210-p3448-0002-j202.dtb}
out=${2:-/boot/j202/nano-j202.dtb}
base=0x83100000 span=0x100000
emc=/external-memory-controller@7001b000
die() { echo "error: $*" >&2; exit 1; }
for c in dtc fdtget fdtput python3; do command -v $c >/dev/null || die "needs $c (pacman -S dtc python)"; done
grep -qi "^ *${base#0x}-831fffff : reserved" /proc/iomem \
  || die "0x83100000 is not reserved: boot once with a J202 DTB that has the cboot-dt reserved-memory node"

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

python3 - "$base" "$span" "$tmp/cboot.dtb" <<'EOF'
import mmap, os, struct, sys
base, span, out = int(sys.argv[1], 16), int(sys.argv[2], 16), sys.argv[3]
fd = os.open("/dev/mem", os.O_RDONLY | os.O_SYNC)
m = mmap.mmap(fd, span, mmap.MAP_SHARED, mmap.PROT_READ, offset=base)
magic, size = struct.unpack(">II", m[:8])
if magic != 0xd00dfeed or not 0 < size <= span:
    sys.exit(f"no device tree at {base:#x} (magic {magic:#x}, size {size})")
open(out, "wb").write(m[:size])
EOF

dtc -q -I dtb -O dts -o "$tmp/cboot.dts" "$tmp/cboot.dtb"

# The table for this RAM code (4 GB module: 0; override with RAM_CODE=).
code=${RAM_CODE:-0}
node="emc-table@$code"
awk -v n="$node" '$0 ~ "^\t\t" n " \\{" {p=1} p {print; if ($0 ~ /^\t\t\};/) exit}' "$tmp/cboot.dts" \
  | grep -v phandle > "$tmp/table.dtsi"
[[ -s $tmp/table.dtsi ]] || die "no $node in cboot's device tree"
grep -q 'nvidia,trained = <0x01>' "$tmp/table.dtsi" || die "$node has no trained table"

dtc -q -I dtb -O dts -o "$tmp/in.dts" "$in"
{
  cat "$tmp/in.dts"
  printf '/ { external-memory-controller@7001b000 { #address-cells = <1>; #size-cells = <0>;\n'
  cat "$tmp/table.dtsi"
  printf '}; };\n'
} > "$tmp/out.dts"
dtc -q -I dts -O dtb -o "$tmp/out.dtb" "$tmp/out.dts"
fdtget -l "$tmp/out.dtb" "$emc/$node" | grep -q 1600000 || die "1600 MHz table missing in the result"
# The PMIC cuts the power at power-off (mainline only halts); its RTC alarm can power the board on.
fdtput "$tmp/out.dtb" /i2c@7000d000/pmic@3c system-power-controller
fdtget -p "$tmp/out.dtb" /i2c@7000d000/pmic@3c | grep -qx system-power-controller || die "system-power-controller not set"
bash "$(dirname "$0")/fix-emmc-dtb.sh" "$tmp/out.dtb"
bash "$(dirname "$0")/fix-video-dtb.sh" "$tmp/out.dtb"
install -Dm644 "$tmp/out.dtb" "$out"
echo "wrote $out: $(fdtget -l "$out" "$emc/$node" | tr '\n' ' ')"

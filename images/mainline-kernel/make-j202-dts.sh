#!/bin/bash
# Add the reComputer J1020 v2 (Nano eMMC module + Seeed J202) device tree to a
# mainline kernel source tree, next to tegra210-p3450-0000.dts which it includes.
#
# Usage: bash make-j202-dts.sh /path/to/linux-source
set -euo pipefail

KDIR=${1:?usage: make-j202-dts.sh <linux-source-dir>}
here=$(cd "$(dirname "$0")" && pwd)
dtsdir="$KDIR/arch/arm64/boot/dts/nvidia"
name=tegra210-p3448-0002-j202

[[ -f $dtsdir/tegra210-p3450-0000.dts ]] || { echo "missing $dtsdir/tegra210-p3450-0000.dts" >&2; exit 1; }
install -m 0644 "$here/j202/$name.dts" "$dtsdir/$name.dts"

if ! grep -q "$name.dtb" "$dtsdir/Makefile"; then
  printf 'dtb-$(CONFIG_ARCH_TEGRA_210_SOC) += %s.dtb\n' "$name" >> "$dtsdir/Makefile"
fi

echo "installed $dtsdir/$name.dts"
echo "build: make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- nvidia/$name.dtb"

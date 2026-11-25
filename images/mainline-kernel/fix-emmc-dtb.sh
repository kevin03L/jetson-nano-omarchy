#!/bin/bash
# J202 device trees: the module's eMMC at 51 MHz on PLL_P, in the mode U-Boot uses. Run on a device
# tree, again is harmless:
#   fix-emmc-dtb.sh FILE.dtb
#
# It only holds the boot files. In HS200 on PLL_C4 the clock of its controller went through 1 GHz
# while it was probed, and the board reset there, boot after boot, with every entry (2026-11-25).
set -euo pipefail
dtb=${1:?usage: $0 FILE.dtb}
emmc=/mmc@700b0600
die() { echo "error: $*" >&2; exit 1; }
for c in fdtget fdtput; do command -v $c >/dev/null || die "needs $c (pacman -S dtc)"; done
[[ $(fdtget "$dtb" $emmc status) == okay ]] || die "$dtb has no eMMC"

tmp=$(mktemp "$dtb.XXXXXX"); trap 'rm -f "$tmp"' EXIT
cat "$dtb" > "$tmp"
read -r car clk _ <<<"$(fdtget -t x "$tmp" $emmc clocks)"
[[ $clk == f ]] || die "the clock of the eMMC is not SDMMC4"
fdtput -t x "$tmp" $emmc assigned-clocks $car $clk
fdtput -t x "$tmp" $emmc assigned-clock-parents $car f3	# TEGRA210_CLK_PLL_P
fdtput -t u "$tmp" $emmc assigned-clock-rates 51000000
fdtput -t u "$tmp" $emmc max-frequency 52000000
fdtput -t x "$tmp" $emmc sdhci-caps-mask 7 0	# no SDR50, SDR104 (HS200), DDR50
for p in mmc-hs400-1_8v mmc-hs200-1_8v; do
  if fdtget -p "$tmp" $emmc | grep -qx $p; then fdtput -d "$tmp" $emmc $p; fi
done
cmp -s "$tmp" "$dtb" && { echo "$dtb: nothing to do"; exit 0; }
cat "$tmp" > "$dtb"
echo "$dtb: eMMC at 51 MHz"

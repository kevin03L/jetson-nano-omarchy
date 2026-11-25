#!/bin/bash
# Tegra210 device trees: what the video engines need. Run on a device tree, again is harmless:
#   fix-video-dtb.sh FILE.dtb
#
# - NVDEC answers only when the NVJPG power domain is on too. Without it the first access to its
#   registers stops the whole board until the watchdog resets it. The nvjpg domain has to come
#   before nvdec, the PMC driver adds the domains in the order of the device tree.
# - VIC (copies a decoded picture to memory the CPU can read) stays at 19.2 MHz: 408 MHz.
#
# Without the first one vaapi-tegra refuses to decode, so every boot entry stays safe.
set -euo pipefail
dtb=${1:?usage: $0 FILE.dtb}
pg=/pmc@7000e400/powergates vic=/host1x@50000000/vic@54340000
die() { echo "error: $*" >&2; exit 1; }
for c in fdtget fdtput; do command -v $c >/dev/null || die "needs $c (pacman -S dtc)"; done
fdtget -p "$dtb" $vic >/dev/null && fdtget -p "$dtb" $pg/nvdec >/dev/null && fdtget -p "$dtb" $pg/nvjpg >/dev/null \
  || die "$dtb has no vic, nvdec or nvjpg"

tmp=$(mktemp "$dtb.XXXXXX"); trap 'rm -f "$tmp"' EXIT
cat "$dtb" > "$tmp"
prop() { fdtget -t x "$tmp" "$@"; }
if ! fdtget -p "$tmp" $pg/nvdec | grep -qx power-domains; then
  clocks=$(prop $pg/nvjpg clocks) resets=$(prop $pg/nvjpg resets) phandle=$(prop $pg/nvjpg phandle)
  cells=$(prop $pg/nvjpg '#power-domain-cells')
  [[ $(fdtget -p "$tmp" $pg/nvjpg | sort | tr '\n' ' ') == '#power-domain-cells clocks phandle resets ' ]] \
    || die "the nvjpg power domain is not as expected"
  # a node added by fdtput comes first in its parent
  fdtput -r "$tmp" $pg/nvjpg
  fdtput -c "$tmp" $pg/nvjpg
  fdtput -t x "$tmp" $pg/nvjpg clocks $clocks
  fdtput -t x "$tmp" $pg/nvjpg resets $resets
  fdtput -t x "$tmp" $pg/nvjpg '#power-domain-cells' $cells
  fdtput -t x "$tmp" $pg/nvjpg phandle $phandle
  fdtput -t x "$tmp" $pg/nvdec power-domains $phandle
fi
if ! fdtget -p "$tmp" $vic | grep -qx assigned-clocks; then
  read -r car clk <<<"$(prop $vic clocks)"
  fdtput -t x "$tmp" $vic assigned-clocks $car $clk
  fdtput -t x "$tmp" $vic assigned-clock-parents $car f3	# TEGRA210_CLK_PLL_P
  fdtput -t u "$tmp" $vic assigned-clock-rates 408000000
fi
order=$(fdtget -l "$tmp" $pg)
(( $(grep -nx nvjpg <<<"$order" | cut -d: -f1) < $(grep -nx nvdec <<<"$order" | cut -d: -f1) )) \
  || die "nvjpg is not before nvdec"
cmp -s "$tmp" "$dtb" && { echo "$dtb: nothing to do"; exit 0; }
cat "$tmp" > "$dtb"
echo "$dtb: video engines set"

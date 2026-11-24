#!/usr/bin/env bash
# Move the desktop from provision-nano.sh's software rendering (llvmpipe) to the GPU. Run by
# scripts/build-nano-stack.sh once mesa-tegra, nvidia-l4t-r36 and nvgpu-nano are installed.
#   /etc/environment      drops MESA_LOADER_DRIVER_OVERRIDE=kms_swrast and LIBGL_ALWAYS_SOFTWARE=1,
#                         which beat nvgpu-nano's Zink environment
#   SDDM                  starts its compositor through jetson-nano-greeter: SDDM does not pass its
#                         environment on, so the greeter's Hyprland cannot open the GPU without it
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
env_file=${ENV_FILE:-/etc/environment}
root=${DESTROOT:-}
if grep -qE '^(MESA_LOADER_DRIVER_OVERRIDE=kms_swrast|LIBGL_ALWAYS_SOFTWARE=1)$' "$env_file"; then
  [[ -e $env_file.pre-nvgpu ]] || cp -p "$env_file" "$env_file.pre-nvgpu"
  sed -i -e '/^# Jetson Nano (Tegra210): Mesa tegra_dri crashes on render/d' \
    -e '/^# fixed (see \/etc\/sddm.conf.d\/20-jetson-nano.conf)\. This is llvmpipe/d' \
    -e '/^MESA_LOADER_DRIVER_OVERRIDE=kms_swrast$/d' -e '/^LIBGL_ALWAYS_SOFTWARE=1$/d' "$env_file"
fi
install -Dm755 "$here/files/usr/local/bin/jetson-nano-greeter" "$root/usr/local/bin/jetson-nano-greeter"
install -Dm644 "$here/files/nvgpu/etc/sddm.conf.d/20-jetson-nano.conf" "$root/etc/sddm.conf.d/20-jetson-nano.conf"

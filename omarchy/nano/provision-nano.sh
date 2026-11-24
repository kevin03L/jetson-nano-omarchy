#!/bin/bash
set -euo pipefail

if [[ "$(id -u)" -ne 0 ]]; then
  printf 'Run this script as root.\n' >&2
  exit 1
fi
if [[ "$(uname -m)" != aarch64 ]]; then
  printf 'This script requires aarch64.\n' >&2
  exit 1
fi

FILES_DIR="$(cd "$(dirname "$0")" && pwd)/files"
USER_NAME="${USER_NAME:-omarchy}"
STAGE_DIR=/var/lib/jetson-nano
LOG_FILE=/var/log/jetson-nano-provision.log
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib-extlinux.sh"
exec > >(tee -a "$LOG_FILE") 2>&1

install_file() {
  local src_rel="$1"
  local mode="$2"
  install -o root -g root -D -m "$mode" "$FILES_DIR/$src_rel" "/$src_rel"
}

# reComputer J1020 v2 (eMMC module on the Seeed J202, fork kevin03L/jetson-nano-omarchy): root on
# the NVMe SSD, /boot on the eMMC, booted by linux-nano (pwm-tegra built in) through its own
# extlinux entries. The SD-card steps (root=LABEL=ALARM, initramfs-linux.img, pwm-tegra DKMS for
# the stock kernel) do not apply.
is_j202() { tr '\0' '\n' < /proc/device-tree/compatible 2>/dev/null | grep -qx 'seeed,recomputer-j1020-v2'; }

as_user() {
  runuser -u "$USER_NAME" -- env -u XDG_RUNTIME_DIR OMARCHY_PATH=/usr/share/omarchy PATH="/usr/share/omarchy/bin:$PATH" "$@" </dev/null
}

as_user_with_path() {
  local omarchy_path="$1"
  shift
  runuser -u "$USER_NAME" -- env -u XDG_RUNTIME_DIR OMARCHY_PATH="$omarchy_path" PATH="$omarchy_path/bin:$PATH" "$@" </dev/null
}

stage_1() {
  local alarm_uid
  local user_uid
  printf 'stage 1: accounts and sudo\n'
  if getent passwd alarm >/dev/null && getent passwd "$USER_NAME" >/dev/null; then
    alarm_uid="$(getent passwd alarm | cut -d: -f3)"
    user_uid="$(getent passwd "$USER_NAME" | cut -d: -f3)"
    if [[ "$alarm_uid" == "$user_uid" ]]; then
      for file in /etc/passwd /etc/shadow /etc/group /etc/gshadow; do
        if [[ ! -e "$file.pre-dedupe" ]]; then
          cp -p "$file" "$file.pre-dedupe"
        fi
        sed -i '/^alarm:/d' "$file"
      done
      sed -i -E '/^wheel:/ { s/(:|,)alarm(,|$)/\1/g; s/,,+/,/g; s/:,/:/g; s/,$//; }' /etc/group /etc/gshadow
      rm -rf /home/alarm
    fi
  fi
  # GPU nodes are group video (nvgpu-nano); the seat ACL covers only the logged-in desktop, so
  # CUDA over ssh or from services needs the group
  usermod -aG video "$USER_NAME"
  pacman -S --needed --noconfirm sudo
  install_file etc/sudoers.d/10-wheel 0440
  visudo -c
  if [[ -s "/home/$USER_NAME/.ssh/authorized_keys" ]]; then
    install_file etc/ssh/sshd_config.d/10-key-only.conf 0644
  else
    printf 'WARNING: no authorized_keys for %s; keeping password SSH login enabled.\n' "$USER_NAME"
  fi
  systemctl reload sshd || true
}

stage_2() {
  printf 'stage 2: base update and build tools\n'
  pacman-key --init
  pacman-key --populate archlinuxarm
  pacman -Syu --noconfirm
  if is_j202; then
    pacman -S --needed --noconfirm base-devel
  else
    pacman -S --needed --noconfirm base-devel dkms linux-aarch64-headers
  fi
}

stage_3() {
  local extlinux=/boot/extlinux/extlinux.conf
  printf 'stage 3: boot: initramfs, guard, extlinux\n'
  install_file etc/mkinitcpio.conf.d/zz-jetson-nano.conf 0644
  install_file usr/local/bin/jetson-nano-boot-check 0755
  install_file etc/pacman.d/hooks/95-jetson-nano-boot-check.hook 0644
  mkinitcpio -P
  systemctl enable systemd-pstore.service
  /usr/local/bin/jetson-nano-boot-check
  if [[ ! -e "$extlinux.pre-provision" ]]; then
    cp -p "$extlinux" "$extlinux.pre-provision"
  fi
  if is_j202; then
    printf 'J202: extlinux.conf left as is (entries written by hand, see docs/j202-plan.md)\n'
    grep -E '^(DEFAULT|LABEL)' "$extlinux"
    return 0
  fi
  edit_extlinux "$extlinux"
  grep -q 'INITRD /boot/initramfs-linux.img' "$extlinux"
  grep -q 'root=LABEL=ALARM' "$extlinux"
  grep '^[[:space:]]*APPEND[[:space:]]' "$extlinux"
}

stage_4() {
  local kver_pkg
  local src
  local package_version
  printf 'stage 4: pwm-tegra DKMS for the stock kernel (the GPU voltage regulator needs it)\n'
  if is_j202; then
    printf 'J202: skipped, linux-nano has pwm-tegra\n'
    return 0
  fi
  kver_pkg="$(pacman -Q linux-aarch64 | awk '{print $2}' | sed 's/-[^-]*$//')"
  src="/usr/src/pwm-tegra-$kver_pkg"
  mkdir -p "$src"
  install_file usr/src/pwm-tegra-template/Makefile 0644
  install_file usr/src/pwm-tegra-template/dkms.conf 0644
  cp /usr/src/pwm-tegra-template/Makefile "$src/Makefile"
  package_version="PACKAGE_VERSION=\"$kver_pkg\""
  sed "s/^PACKAGE_VERSION=.*/$package_version/" /usr/src/pwm-tegra-template/dkms.conf > "$src/dkms.conf"
  curl -fsSL "https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/drivers/pwm/pwm-tegra.c?h=v$kver_pkg" -o "$src/pwm-tegra.c"
  if ! head -1 "$src/pwm-tegra.c" | grep -q '^// SPDX-License-Identifier: GPL-2.0'; then
    printf 'Downloaded pwm-tegra.c has an unexpected SPDX header.\n' >&2
    exit 1
  fi
  if dkms status "pwm-tegra/$kver_pkg" | grep -q installed; then
    printf 'pwm-tegra/%s is already installed.\n' "$kver_pkg"
  else
    dkms install "pwm-tegra/$kver_pkg"
  fi
  modprobe pwm-tegra || true
}

stage_5() {
  printf 'stage 5: fan\n'
  install_file usr/local/bin/jetson-nano-fan 0755
  install_file etc/systemd/system/jetson-nano-fan.service 0644
  systemctl daemon-reload
  systemctl enable --now jetson-nano-fan.service
}

stage_6() {
  local omarchy_path=/usr/share/omarchy
  printf 'stage 6: Omarchy repositories and packages\n'
  pacman -S --needed --noconfirm git
  if [[ ! -d /usr/share/omarchy ]]; then
    # outside /root: the scripts run as the desktop user
    rm -rf /var/lib/jetson-nano/omarchy-git-v4.5.0
    git clone --depth 1 --branch v4.5.0 https://github.com/basecamp/omarchy /var/lib/jetson-nano/omarchy-git-v4.5.0
    omarchy_path=/var/lib/jetson-nano/omarchy-git-v4.5.0
  fi
  install -o root -g root -m 0440 /dev/null /etc/sudoers.d/90-provision-temp
  printf '%s ALL=(ALL) NOPASSWD: ALL\n' "$USER_NAME" > /etc/sudoers.d/90-provision-temp
  trap 'rm -f /etc/sudoers.d/90-provision-temp' EXIT
  visudo -c
  as_user_with_path "$omarchy_path" omarchy-refresh-pacman stable
  as_user_with_path "$omarchy_path" omarchy-update-keyring
  pacman -Syy
  if pacman -Q openssl-3 >/dev/null 2>&1; then
    pacman -Suu --noconfirm --overwrite /usr/lib/libcrypto.so.3 --overwrite /usr/lib/libssl.so.3
    pacman -Rdd --dbonly --noconfirm openssl-3
  else
    pacman -Suu --noconfirm
  fi
  if [[ -d /usr/share/omarchy ]] && ! pacman -Qo /usr/share/omarchy >/dev/null 2>&1; then
    mv /usr/share/omarchy "/root/omarchy-git-unpackaged-$(date +%s)"
    find /usr/bin -maxdepth 1 -lname '/usr/share/omarchy/*' -delete
  fi
  pacman -S --needed --noconfirm omarchy omarchy-settings
  as_user omarchy-reinstall-pkgs
  /usr/local/bin/jetson-nano-boot-check
}

stage_7() {
  local output=/tmp/jetson-nano-system-setup.out
  local command_rc
  printf 'stage 7: Omarchy system setup\n'
  if command -v ufw >/dev/null 2>&1; then
    ufw allow 22/tcp
  fi
  # Omarchy's hardware stage runs install/hardware/tegra210.sh, guarded by the platform detection;
  # both live in this repository (install-omarchy-nano.sh installs them the same way)
  install -Dm755 "$SCRIPT_DIR/../platform-detection/omarchy-hw-aarch64-tegra210" /usr/local/bin/omarchy-hw-aarch64-tegra210
  install -Dm755 "$SCRIPT_DIR/../installer-hooks/tegra210.sh" /usr/share/omarchy/install/hardware/tegra210.sh
  install_file root/omarchy-apply-system-nano.sh 0755
  if bash /root/omarchy-apply-system-nano.sh 2>&1 | tee "$output"; then
    command_rc=0
  else
    command_rc=$?
  fi
  if [[ "$command_rc" -ne 0 ]] || grep 'rc=' "$output" | grep -Ev 'rc=0([[:space:]]|$)' >/dev/null; then
    printf 'Omarchy system setup reported a failed stage.\n' >&2
    return 1
  fi
  if [[ -f /var/log/omarchy-install.log ]]; then
    grep 'Failed:' /var/log/omarchy-install.log || true
  fi
  install_file etc/sddm.conf.d/20-jetson-nano.conf 0644
  if [[ ! -e /etc/environment.pre-jetson ]]; then
    cp -p /etc/environment /etc/environment.pre-jetson
  fi
  tmp="$(mktemp)"
  awk '
    /^# Jetson Nano \(Tegra210\): Mesa tegra_dri crashes on render; use software GL until$/ { next }
    /^# fixed \(see \/etc\/sddm.conf.d\/20-jetson-nano.conf\)\. This is llvmpipe, NOT GPU\.$/ { next }
    !inserted && /^(AQ_DRM_DEVICES|MESA_LOADER_DRIVER_OVERRIDE|LIBGL_ALWAYS_SOFTWARE)=/ {
      print "# Jetson Nano (Tegra210): Mesa tegra_dri crashes on render; use software GL until"
      print "# fixed (see /etc/sddm.conf.d/20-jetson-nano.conf). This is llvmpipe, NOT GPU."
      inserted=1
    }
    { print }
    END {
      if (!inserted) {
        print "# Jetson Nano (Tegra210): Mesa tegra_dri crashes on render; use software GL until"
        print "# fixed (see /etc/sddm.conf.d/20-jetson-nano.conf). This is llvmpipe, NOT GPU."
      }
    }
  ' /etc/environment > "$tmp"
  cat "$tmp" > /etc/environment
  rm -f "$tmp"
  if ! grep -q '^AQ_DRM_DEVICES=' /etc/environment; then
    printf '%s\n' 'AQ_DRM_DEVICES=/dev/dri/by-path/platform-50000000.host1x-card' >> /etc/environment
  fi
  if ! grep -q '^MESA_LOADER_DRIVER_OVERRIDE=' /etc/environment; then
    printf '%s\n' 'MESA_LOADER_DRIVER_OVERRIDE=kms_swrast' >> /etc/environment
  fi
  if ! grep -q '^LIBGL_ALWAYS_SOFTWARE=' /etc/environment; then
    printf '%s\n' 'LIBGL_ALWAYS_SOFTWARE=1' >> /etc/environment
  fi
  install -d -o sddm -g sddm /var/lib/sddm
  if [[ ! -e /var/lib/sddm/state.conf ]]; then
    cat > /var/lib/sddm/state.conf <<EOF
[Last]
User=$USER_NAME
Session=/usr/local/share/wayland-sessions/omarchy.desktop
EOF
    chown sddm:sddm /var/lib/sddm/state.conf
  fi
}

stage_8() {
  printf 'stage 8: network\n'
  # Static address only when NANO_ADDRESS (CIDR, e.g. 192.0.2.10/24) and NANO_GATEWAY are set;
  # otherwise DHCP. NANO_DNS defaults to the gateway plus 1.1.1.1.
  local conn=etc/NetworkManager/system-connections/wired-nano.nmconnection
  if [[ -n ${NANO_ADDRESS:-} ]]; then
    [[ -n ${NANO_GATEWAY:-} ]] || { printf 'NANO_ADDRESS needs NANO_GATEWAY\n' >&2; return 1; }
    sed -e "s|@NANO_ADDRESS@|$NANO_ADDRESS|" -e "s|@NANO_GATEWAY@|$NANO_GATEWAY|" \
        -e "s|@NANO_DNS@|${NANO_DNS:-$NANO_GATEWAY;1.1.1.1;}|" "$FILES_DIR/$conn" |
      install -o root -g root -D -m 0600 /dev/stdin "/$conn"
  else
    sed -e '/^address1=/d' -e '/^dns=/d' -e 's/^method=manual/method=auto/' "$FILES_DIR/$conn" |
      install -o root -g root -D -m 0600 /dev/stdin "/$conn"
  fi
  systemctl disable dhcpcd systemd-networkd 2>/dev/null || true
  systemctl enable NetworkManager
  printf 'network takes effect at next boot: %s\n' "${NANO_ADDRESS:-DHCP}"
}

stage_9() {
  local monitors="/home/$USER_NAME/.config/hypr/monitors.lua"
  local looknfeel="/home/$USER_NAME/.config/hypr/looknfeel.lua"
  local tmp
  printf 'stage 9: user desktop config\n'
  if [[ ! -e "$monitors" ]]; then
    as_user bash -c 'cp -af /etc/skel/. ~/'
  fi
  # Monitor: the HDMI path gives no EDID on mainline (hot-plug needs the HDMI 5 V, whose enable
  # pin breaks boot when driven from the device tree; docs/attempts), so the kernel loads the
  # monitor's EDID from /usr/lib/firmware/edid. Use its native (preferred) mode at scale 1: sharp,
  # and cheaper than fractional scaling. An older provision pinned 1280x720; replace that rule.
  if [[ -f "$monitors" ]]; then
    sed -i -e '/Jetson Nano: the monitor gives no EDID/d' \
      -e '/^hl\.monitor({ output = "HDMI-A-1", mode = "1280x720@60"/d' \
      -e 's/^local omarchy_gdk_scale = 2$/local omarchy_gdk_scale = 1/' "$monitors"
    if ! grep -q 'output = "HDMI-A-1"' "$monitors"; then
      tmp="$(mktemp)"
      awk '
        !inserted && /^hl\.monitor\(\{ output = "",/ {
          print
          print "-- Jetson Nano: native mode of the HDMI monitor (EDID from /usr/lib/firmware/edid), no fractional scaling."
          print "hl.monitor({ output = \"HDMI-A-1\", mode = \"preferred\", position = \"0x0\", scale = 1 })"
          inserted=1
          next
        }
        { print }
        END { if (!inserted) exit 1 }
      ' "$monitors" > "$tmp"
      cat "$tmp" > "$monitors"
      rm -f "$tmp"
    fi
  fi
  # Look: Omarchy's stock look (animations on; blur and shadows as the theme sets them). An older
  # provision switched everything off for software rendering; the desktop now renders on the GPU.
  # Blur, when a theme turns it on, uses xray mode: blurring only the cached wallpaper keeps it at
  # about the GPU cost of no blur at 2560x1440.
  if [[ -f "$looknfeel" ]]; then
    sed -i -e '/^hl\.config({ decoration = { blur = { enabled = false }, shadow = { enabled = false } }, animations = { enabled = false } })$/d' \
      -e '/^-- Jetson Nano: software rendering until the Mesa tegra fix lands/d' \
      -e '/^-- shadows or animations: each one multiplies the per-frame CPU cost\.$/d' \
      -e '/^-- Jetson Nano: the desktop renders on the GPU through the patched Mesa tegra$/,/^-- CONFIG_TEGRA210_EMC\.$/d' \
      -e '/^-- The Nano leaves black rectangles behind the pointer/,/^-- while the GPU is at its idle clock\.$/d' "$looknfeel"
    if ! grep -q 'xray = true' "$looknfeel"; then
      cat >> "$looknfeel" <<'EOF'

-- Jetson Nano: blur (when a theme enables it) blurs only the cached wallpaper.
hl.config({ decoration = { blur = { xray = true, new_optimizations = true } } })
EOF
    fi
  fi
  # Omarchy's screensaver animates a terminal at 120 fps: more than a CPU core on the Nano while
  # idle. Turn it off with Omarchy's own toggle (the Omarchy menu turns it back on).
  as_user bash -c 'mkdir -p ~/.local/state/omarchy/toggles && touch ~/.local/state/omarchy/toggles/screensaver-off'
  if [[ -f "$looknfeel" ]] && ! grep -q 'use_cpu_buffer' "$looknfeel"; then
    cat >> "$looknfeel" <<'EOF'

-- Jetson Nano: hardware cursor. The Tegra210 cursor plane takes RGBA8888 only, which the
-- Nano's Hyprland build (packaging/hyprland-nano) writes from a CPU buffer.
hl.config({ cursor = { use_cpu_buffer = 1 } })
EOF
  fi
  if [[ -d "/home/$USER_NAME/.config/hypr" ]]; then
    chown -R "$USER_NAME:$USER_NAME" "/home/$USER_NAME/.config/hypr"
  fi
  printf 'PROVISION DONE: reboot to start the Omarchy login (SDDM)\n'
}

run_stage() {
  local stage="$1"
  local marker="$STAGE_DIR/stage-$stage.done"
  mkdir -p "$STAGE_DIR"
  if [[ -e "$marker" ]]; then
    printf 'stage %s already done\n' "$stage"
    return 0
  fi
  "stage_$stage"
  touch "$marker"
}

usage() {
  printf 'Usage: %s [--from N | --only N]\n' "$0" >&2
  exit 2
}

mode=all
selected_stage=
if [[ "$#" -gt 0 ]]; then
  [[ "$#" -eq 2 ]] || usage
  case "$1" in
    --from|--only) mode="${1#--}" ;;
    *) usage ;;
  esac
  selected_stage="$2"
  [[ "$selected_stage" =~ ^[1-9]$ ]] || usage
fi

if [[ "$mode" == from ]]; then
  mkdir -p "$STAGE_DIR"
  for stage in {1..9}; do
    if (( stage >= selected_stage )); then
      rm -f "$STAGE_DIR/stage-$stage.done"
    fi
  done
  for stage in {1..9}; do
    if (( stage >= selected_stage )); then
      run_stage "$stage"
    fi
  done
elif [[ "$mode" == only ]]; then
  run_stage "$selected_stage"
else
  for stage in {1..9}; do
    run_stage "$stage"
  done
fi

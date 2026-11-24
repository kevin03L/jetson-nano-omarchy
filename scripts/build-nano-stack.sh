#!/usr/bin/env bash
# Build and install the Jetson Nano stack on the Nano itself, in dependency order: the
# linux-nano kernel, NVIDIA's GM20B firmware, R36 userspace and nvgpu driver, the Nano builds of
# Mesa, aquamarine, Hyprland and Omarchy, the board policy, and the CUDA toolchain with nanoblas.
#
# Every package goes into a local pacman repository, [nano], listed first in pacman.conf.
# pacman takes a package from the first repository that has it, whatever the version, so
# `pacman -Syu` and `omarchy-update` keep the Nano builds instead of replacing them with the
# generic ones. Then the script adds the boot entries and makes linux-nano's device tree.
#
# Run as root on the Nano from a checkout of this repository, after omarchy/nano/provision-nano.sh:
#   scripts/build-nano-stack.sh [--dry-run] [--only DIR] [--from DIR] [--rebuild] [--no-boot]
# DIR is a directory under packaging/. A package whose files are already in the repository is
# not built again (unless --rebuild), so the script resumes where it stopped.
#
# Environment: BUILD_USER (default omarchy) builds the packages; makepkg never runs as root.
# REPO_DIR (default /var/lib/jetson-nano/repo), WORK_DIR (default /var/tmp/nano-stack).
# On the Nano alone the kernel and gcc13 take many hours; distcc to a faster aarch64 or
# cross-distcc host is the usual way to shorten that (see makepkg.conf BUILDENV and DISTCC_HOSTS).
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
BUILD_USER=${BUILD_USER:-omarchy}
REPO_DIR=${REPO_DIR:-/var/lib/jetson-nano/repo}
WORK_DIR=${WORK_DIR:-/var/tmp/nano-stack}
PACMAN_CONF=${PACMAN_CONF:-/etc/pacman.conf}
EXTLINUX=${EXTLINUX:-/boot/extlinux/extlinux.conf}
REPO_NAME=nano

# Order matters: each package's dependencies come before it.
ORDER=(nvidia-l4t-gm20b-firmware linux-nano nvidia-l4t-r36 mesa-tegra nvgpu-nano aquamarine-nano
  hyprland-nano omarchy-nano jetson-nano-board gcc13 cuda-nano nanoblas)

DRY=0 ONLY="" FROM="" REBUILD=0 BOOT=1
while (($#)); do
  case $1 in
    --dry-run) DRY=1 ;;
    --only) ONLY=$2; shift ;;
    --from) FROM=$2; shift ;;
    --rebuild) REBUILD=1 ;;
    --no-boot) BOOT=0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

die() { echo "error: $*" >&2; exit 1; }
say() { printf '== %s %s\n' "$(date +%T)" "$*"; }

[[ $EUID -eq 0 ]] || die "run as root"
id "$BUILD_USER" >/dev/null 2>&1 || die "no user $BUILD_USER (set BUILD_USER)"
for d in "${ORDER[@]}"; do [[ -f $here/packaging/$d/PKGBUILD ]] || die "missing packaging/$d/PKGBUILD"; done
if [[ -n $ONLY ]]; then ORDER=("$ONLY"); fi
if [[ -n $FROM ]]; then
  i=0; for d in "${ORDER[@]}"; do [[ $d == "$FROM" ]] && break; i=$((i + 1)); done
  ((i < ${#ORDER[@]})) || die "--from $FROM: not in the build order"
  ORDER=("${ORDER[@]:i}")
fi

as_user() { runuser -u "$BUILD_USER" -- "$@"; }

# srcinfo field values for one key, from makepkg --printsrcinfo output on stdin
field() { awk -v k="$1" -F' = ' '$1 ~ "^[ \t]*"k"$" {print $2}'; }

install -d -m 755 "$REPO_DIR"
install -d -o "$BUILD_USER" -m 755 "$WORK_DIR"

build_one() {
  local dir=$1 w="$WORK_DIR/$1" info ver epoch names files missing f
  rm -rf "$w"
  cp -a "$here/packaging/$dir" "$w"
  chown -R "$BUILD_USER": "$w"
  info=$(cd "$w" && as_user makepkg --printsrcinfo)
  epoch=$(field epoch <<<"$info" | head -1)
  ver="${epoch:+$epoch:}$(field pkgver <<<"$info" | head -1)-$(field pkgrel <<<"$info" | head -1)"
  names=$(field pkgname <<<"$info")
  mapfile -t files < <(cd "$w" && as_user makepkg --packagelist | xargs -n1 basename)
  local have=1
  for f in "${files[@]}"; do [[ -f $REPO_DIR/$f ]] || have=0; done
  if ((have && !REBUILD)); then
    say "$dir $ver: in the repository"
  else
    # dependencies that are not installed yet, minus those this PKGBUILD makes itself (gcc13-libs);
    # the build order provides our other packages
    mapfile -t missing < <(pacman -T $(field depends <<<"$info") $(field makedepends <<<"$info") \
      $(field checkdepends <<<"$info") | grep -vxFf <(echo "$names") || true)
    if ((DRY)); then
      say "$dir $ver: would build ($(echo $names)); missing dependencies: ${missing[*]:-none}"
      return
    fi
    if ((${#missing[@]})); then
      say "$dir: installing dependencies ${missing[*]}"
      pacman -S --needed --asdeps --noconfirm "${missing[@]}"
    fi
    # a source that is itself a pacman package (omarchy-nano) comes from pacman's cache; makepkg
    # still checks it against the PKGBUILD's sha256
    for f in $(field source <<<"$info"); do
      [[ $f == *.pkg.tar.* && $f != *://* && ! -e $w/$f ]] || continue
      [[ -f /var/cache/pacman/pkg/$f ]] || pacman -Sw --noconfirm "${f%-*-*-*}" || true
      [[ -f /var/cache/pacman/pkg/$f ]] || die "$dir: $f is not in /var/cache/pacman/pkg (or the repositories)"
      install -o "$BUILD_USER" -m 644 "/var/cache/pacman/pkg/$f" "$w/$f"
    done
    # nanoblas and nvgpu checks need the GPU; skip check() when nvgpu is not running yet
    local nocheck=()
    [[ -e /dev/nvhost-ctrl-gpu ]] || nocheck=(--nocheck)
    say "$dir $ver: building (log $w/build.log)"
    (cd "$w" && as_user makepkg -f --noconfirm "${nocheck[@]}" >build.log 2>&1) \
      || { tail -30 "$w/build.log"; die "$dir: build failed"; }
    for f in "${files[@]}"; do install -m 644 "$w/$f" "$REPO_DIR/$f"; done
    repo-add -q -R "$REPO_DIR/$REPO_NAME.db.tar.gz" "${files[@]/#/$REPO_DIR/}"
  fi
  ((DRY)) && return
  # install, or upgrade, every package the PKGBUILD makes; already current ones are skipped
  local todo=() n
  for n in $names; do
    [[ $(pacman -Q "$n" 2>/dev/null | cut -d' ' -f2) == "$ver" ]] || todo+=("$n")
  done
  if ((${#todo[@]})); then
    say "$dir: installing ${todo[*]}"
    local paths=()
    for f in "${files[@]}"; do
      for n in "${todo[@]}"; do [[ $f == "$n-$ver-"* ]] && paths+=("$REPO_DIR/$f"); done
    done
    pacman -U --noconfirm "${paths[@]}"
  fi
}

add_repo() {
  # [nano] goes before the first repository section, so its packages win
  if grep -q "^\[$REPO_NAME\]" "$PACMAN_CONF"; then return; fi
  cp -p "$PACMAN_CONF" "$PACMAN_CONF.pre-nano"
  awk -v r="$REPO_NAME" -v d="$REPO_DIR" '
    !done && /^\[/ && $0 != "[options]" {
      print "# Jetson Nano builds (scripts/build-nano-stack.sh). First, so these packages win."
      print "[" r "]"; print "SigLevel = Optional TrustAll"; print "Server = file://" d; print ""
      done = 1
    }
    { print }' "$PACMAN_CONF.pre-nano" >"$PACMAN_CONF"
  say "pacman.conf: [$REPO_NAME] added before the other repositories"
}

for d in "${ORDER[@]}"; do build_one "$d"; done
((DRY)) && exit 0

[[ -f $REPO_DIR/$REPO_NAME.db.tar.gz ]] || repo-add -q "$REPO_DIR/$REPO_NAME.db.tar.gz"
add_repo
pacman -Sy

if ((BOOT)); then
  # shellcheck source=../omarchy/nano/lib-extlinux.sh
  . "$here/omarchy/nano/lib-extlinux.sh"
  if tr '\0' '\n' < /proc/device-tree/compatible | grep -qx 'seeed,recomputer-j1020-v2'; then
    # reComputer J1020 v2: the J202 DTB with cboot's EMC tables (make-j202-nano-dtb.sh), and the
    # kernel arguments of the hand-written linux-nano entry (LABEL nano-plain, NVMe root)
    live=/boot/j202/nano-j202.dtb
    nano_dtb=/boot/j202/nano-j202-cuda.dtb
    src_label=nano-plain
  else
    live=/boot/tegra210-p3448-0002-p3449-0000-b00.dtb
    nano_dtb=/boot/tegra210-p3448-0002-p3449-0000-b00-nano.dtb
    src_label=primary
  fi
  [[ -f $live ]] || die "no $live (the card builder copies the board's device tree there)"
  pacman -S --needed --noconfirm dtc
  bash "$here/images/mainline-kernel/make-nano-dtb.sh" "$live" "$nano_dtb"
  [[ -f $EXTLINUX.pre-nano ]] || cp -p "$EXTLINUX" "$EXTLINUX.pre-nano"
  add_nano_entries "$EXTLINUX" "${nano_dtb#/boot/}" "$src_label"
  say "boot entries:"; grep -E '^(DEFAULT|LABEL)' "$EXTLINUX"
fi
say "done: reboot to start the nano-cuda entry (desktop and CUDA on nvgpu)"

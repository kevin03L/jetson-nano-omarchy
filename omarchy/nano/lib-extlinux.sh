#!/bin/bash

edit_extlinux() {
  local file="$1"
  local token
  # panic=10 with hung_task_panic and softlockup_panic: a driver deadlock or a stuck CPU reboots the
  # board instead of hanging it; reserve_mem + ramoops keep the panic log across that reboot
  # (systemd-pstore files it under /var/lib/systemd/pstore).
  local tokens=(loglevel=7 panic=10 hung_task_panic=1 softlockup_panic=1
    reserve_mem=1M:4096:oops ramoops.mem_name=oops ramoops.record_size=0x40000
    clk_ignore_unused pd_ignore_unused video=HDMI-A-1:1280x720@60e)

  sed -E \
    -e '/INITRD \/boot\/initramfs-linux(-nano)?\.img$/! s/^[[:space:]]*INITRD .*/      INITRD \/boot\/initramfs-linux.img/' \
    -e '/^[[:space:]]*APPEND[[:space:]]/ s/(^|[[:space:]])root=\/dev\/mmcblk0p1([[:space:]]|$)/\1root=LABEL=ALARM\2/' \
    -e '/^[[:space:]]*APPEND[[:space:]]/ s/([[:space:]])quiet([[:space:]])/\1/g' \
    -e '/^[[:space:]]*APPEND[[:space:]]/ s/[[:space:]]quiet$//' \
    -e '/^[[:space:]]*APPEND[[:space:]]/ s/video=HDMI-A-1:[^[:space:]]*/video=HDMI-A-1:1280x720@60e/g' \
    -i.bak "$file"
  rm -f "$file.bak"

  for token in "${tokens[@]}"; do
    if ! grep -Eq "(^|[[:space:]])${token}([[:space:]]|$)" <(sed -n '/^[[:space:]]*APPEND[[:space:]]/p' "$file"); then
      sed -E \
        -e "/^[[:space:]]*APPEND[[:space:]]/ s|$| ${token}|" \
        -i.bak "$file"
      rm -f "$file.bak"
    fi
  done
}

# Add the linux-nano boot entries and make nano-cuda the default. Their kernel arguments are the
# primary entry's (after edit_extlinux) plus the driver choice: nano-cuda runs the desktop and
# CUDA on NVIDIA's nvgpu; nano runs the desktop on nouveau, without CUDA, as the fallback. The
# primary entry (the distribution kernel) stays as it is. A file that has nano-cuda is unchanged.
# Usage: add_nano_entries FILE DTB [SOURCE_LABEL]: DTB is relative to /boot; SOURCE_LABEL (default
# primary) is the entry whose kernel arguments are reused.
add_nano_entries() {
  local file="$1" dtb="$2" src="${3:-primary}" append
  grep -q '^LABEL nano-cuda$' "$file" && return 0
  append=$(awk -v src="$src" '$0 == "LABEL " src { p = 1 }
    p && /^[[:space:]]*APPEND[[:space:]]/ { sub(/^[[:space:]]*APPEND[[:space:]]+/, ""); print; exit }' "$file")
  [[ -n $append ]] || { echo "add_nano_entries: no APPEND in LABEL $src of $file" >&2; return 1; }
  {
    printf '\nLABEL nano-cuda\n'
    printf '      MENU LABEL linux-nano: desktop and CUDA on nvgpu\n'
    printf '      LINUX /boot/Image-nano\n'
    printf '      INITRD /boot/initramfs-linux-nano.img\n'
    printf '      FDT /boot/%s\n' "$dtb"
    printf '      APPEND %s modprobe.blacklist=nouveau nano.mode=cuda\n' "$append"
    printf '\nLABEL nano\n'
    printf '      MENU LABEL linux-nano: desktop on nouveau, no CUDA (fallback)\n'
    printf '      LINUX /boot/Image-nano\n'
    printf '      INITRD /boot/initramfs-linux-nano.img\n'
    printf '      FDT /boot/%s\n' "$dtb"
    printf '      APPEND %s nouveau.config=NvClkMode=0x07 modprobe.blacklist=nvgpu\n' "$append"
  } >>"$file"
  if grep -q '^DEFAULT ' "$file"; then
    sed -i.bak -e 's/^DEFAULT .*/DEFAULT nano-cuda/' "$file"
  else
    sed -i.bak -e '1i\
DEFAULT nano-cuda' "$file"
  fi
  rm -f "$file.bak"
}

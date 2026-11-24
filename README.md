# jetson-nano-omarchy

[Omarchy](https://omarchy.org) running natively on the original NVIDIA Jetson Nano 4 GB (Tegra210,
GM20B Maxwell GPU, compute capability 5.3), on a mainline Linux kernel and Arch Linux ARM, with
NVIDIA's own GPU driver ported to that kernel so CUDA 12 can run next to the desktop.

This is a research project. Every result in `docs/` and `results/` is labelled **measured** (seen on
the board, with the log named) or **inferred**. Nothing here is supported by NVIDIA or Omarchy.

## Status (2026-11-22)

| Area | State |
|---|---|
| Omarchy 4.5 desktop (Hyprland, SDDM) | works on **one driver stack**: Mesa Zink on NVIDIA's R36 Vulkan driver over nvgpu; 34 of 36 Omarchy apps open and close cleanly ([docs/cuda-feasibility.md §22](docs/cuda-feasibility.md)) |
| Kernel `linux-nano` 7.2.11-5 | ALARM `linux-aarch64` + Tegra210 config, MGLRU, zram (zstd); warm reboots no longer crash in the XUSB driver (patch 0103) |
| NVIDIA nvgpu + nvmap (L4T R36) | packaged (`nvgpu-nano`), loads at boot without workarounds (GPC clock deadlock fixed); DVFS 76.8 to 921.6 MHz |
| CUDA | **CUDA 12.9** compiler and runtime (`cuda-nano`), the last release that targets sm_53; `nvcc` works end to end with `gcc13` |
| cuBLAS | `nanoblas`: the cuBLAS API subset applications use (GEMM in FP32/FP16/BF16, batched GEMM and TRSM), built for sm_53, since NVIDIA's math libraries ship no sm_53 code |
| Vulkan 1.3 | works (NVIDIA R36 driver); `VK_LAYER_NANO_display` answers the display queries NVIDIA's driver fails on mainline and stops exit crashes |
| Power | `jetson-nano-board` keeps the board inside its supply: clock caps per supply profile, applied the moment the GPU appears, plus a 20 Hz loop on the INA3221 input sensor |
| Self-recovery | hung tasks and soft lockups panic and reboot in 10 s; the PMIC watchdog resets a frozen board; the panic log survives in ramoops |
| Display | native resolution once the monitor's EDID is supplied as firmware; HDMI hot-plug does not work on mainline yet |

Read [docs/omarchy-on-jetson-nano.md](docs/omarchy-on-jetson-nano.md) for the desktop port and
[docs/cuda-feasibility.md](docs/cuda-feasibility.md) for the CUDA work, in that order.

## What you need

- A Jetson Nano 4 GB, B01 carrier board, with its QSPI bootloader. The scripts never write QSPI
  ([docs/boot-chain.md](docs/boot-chain.md)).
- A 5 V barrel-jack supply. **4 A is recommended** (fit the J48 jumper and set `SUPPLY_W=20` in
  `/etc/jetson-nano/power.conf` for full GPU clocks). A 2 A supply works with the default 10 W profile,
  which caps the GPU at 614.4 MHz; micro-USB power browns the board out under desktop load.
- A microSD card of 128 GB (the package builds use about 60 GB), wired Ethernet, and an HDMI monitor.
- Time: the kernel and `gcc13` take many hours to build on the Nano itself; distcc to a faster
  aarch64 host shortens that.

## Install

1. **Card.** On macOS, `scripts/build-sd-image-mac.sh --card /dev/diskN --write` writes Arch Linux ARM
   with a working boot entry (set `ROOT_PASS` and change the password after the first boot; the
   script's header lists its options). From an
   existing aarch64 Linux on the Nano, `scripts/install-nano.sh` does the same to a second card.
2. **Provision.** Boot the card, then as root over SSH run `bash omarchy/nano/provision-nano.sh`. It
   sets up accounts, the boot configuration, Omarchy and the user's desktop, in resumable stages
   ([omarchy/nano/README.md](omarchy/nano/README.md)).
3. **The Nano stack.** As root, `bash scripts/build-nano-stack.sh` builds and installs every package
   in order into a local `[nano]` pacman repository listed first, so updates keep the Nano builds. It
   then makes linux-nano's device tree and adds the boot entries. Run it with `--dry-run` first; it
   resumes where it stopped.
4. **Reboot.** The default entry, `nano-cuda`, starts the desktop and CUDA on nvgpu. `nano` is the
   nouveau fallback and `primary` the distribution kernel.
5. **Check.** `/usr/lib/nanoblas/gemm_test` runs every GEMM and TRSM case on the GPU against a CPU
   reference and should report all of them passed.

## Known issues

- **An extra silent reset on some warm reboots.** About one warm reboot in three resets once more about
  20 s in, with no log, then boots normally. It predates the current kernel and is under investigation.
- **The nouveau fallback entry** times out in its graphics engine under Hyprland. Use it for recovery,
  not as a desktop.
- **Window-surface Vulkan** (`VK_KHR_wayland/xcb/xlib_surface`) is hidden, because NVIDIA's driver hangs
  presenting on mainline. Vulkan apps fall back to OpenGL on Zink; compute and offscreen Vulkan work.
- **CUDA math libraries.** Only the cuBLAS subset in nanoblas exists for sm_53. cuFFT, cuDNN, TensorRT
  and NVIDIA's other libraries cannot run on this GPU with CUDA 12.
- **Power off** halts the board but does not cut power (the PMIC's `system-power-controller` is not in
  the device tree yet).
- **HDMI hot-plug and EDID** do not work on mainline; the mode is forced on the kernel command line.

## Layout

| Path | Contents |
|---|---|
| `docs/` | engineering record, plans, decisions (ADRs), boot chain and recovery |
| `packaging/` | Arch packages: `linux-nano` kernel, `nvgpu-nano`, `nvidia-l4t-r36`, `nvidia-l4t-gm20b-firmware`, `jetson-nano-board`, `hyprland-nano`, `aquamarine-nano`, `mesa-tegra`, `omarchy-nano`, `gcc13`, `cuda-nano`, `nanoblas` |
| `omarchy/` | provisioning, platform detection and config files for the Nano |
| `tools/cuda-e*/` | CUDA experiments: E0 desk checks, E2 nvgpu/nvmap builds, E4 power-on, E5 smoke test |
| `scripts/` | SD-card build, the Nano stack builder (`build-nano-stack.sh`), boot evidence and diagnostics helpers |
| `results/` | logs and measurements (sanitised with `tools/sanitize-log.py`) |
| `tests/` | smoke and regression test plans |

The NVIDIA driver ports live in forks of OE4T's mirrors, branch `mainline-7.2`:
[Kristianaaron/linux-nvgpu](https://github.com/Kristianaaron/linux-nvgpu) and
[Kristianaaron/linux-nv-oot](https://github.com/Kristianaaron/linux-nv-oot).

## Safety

The scripts never write the QSPI bootloader. `scripts/build-sd-image-mac.sh` refuses to overwrite a
card that holds an L4T system and requires `ROOT_PASS` for the initial password (change it after the
first boot). See [docs/rollback-recovery.md](docs/rollback-recovery.md) before touching a card.

## Credits

This builds on other people's work:

| Who | What | Source | Licence |
|---|---|---|---|
| NVIDIA | nvgpu and nvidia-oot drivers, L4T firmware and userspace | [nv-tegra.nvidia.com](https://nv-tegra.nvidia.com) | GPL-2.0 (drivers); NVIDIA licence (firmware, userspace, not redistributed here) |
| OE4T (OpenEmbedded for Tegra) | maintained mirrors and patches of NVIDIA's R36 drivers | [OE4T/linux-nvgpu](https://github.com/OE4T/linux-nvgpu), [OE4T/linux-nv-oot](https://github.com/OE4T/linux-nv-oot) | GPL-2.0 |
| Arch Linux ARM (Kevin Mihelich, graysky) | `linux-aarch64` PKGBUILD, config and patches that `linux-nano` derives from | [archlinuxarm/PKGBUILDs](https://github.com/archlinuxarm/PKGBUILDs) | GPL-2.0 (kernel) |
| Omarchy (DHH and contributors) | the desktop this port runs | [basecamp/omarchy](https://github.com/basecamp/omarchy) | MIT |
| Omarchy packagers | `hyprland` and `aquamarine` package recipes and patches that `hyprland-nano` and `aquamarine-nano` extend | [omacom/omarchy-pkgs](https://github.com/omacom/omarchy-pkgs) | per package |
| hyprwm (Vaxry and contributors) | Hyprland and aquamarine | [hyprwm/Hyprland](https://github.com/hyprwm/Hyprland), [hyprwm/aquamarine](https://github.com/hyprwm/aquamarine) | BSD-3-Clause |
| Mesa, nouveau and Linux Tegra maintainers | GM20B graphics, Tegra210 support, EMC driver (Joseph Lo, Thierry Reding) | [mesa](https://gitlab.freedesktop.org/mesa/mesa), [kernel.org](https://kernel.org) | MIT / GPL-2.0 |

Not affiliated with or endorsed by NVIDIA, Omarchy, Basecamp or Arch Linux ARM.

## Licence

Scripts, tools and documentation: MIT ([LICENSE](LICENSE)). Patches keep the licence of the code
they modify (kernel and drivers GPL-2.0, Mesa and Omarchy MIT). No NVIDIA binaries are included;
the scripts download them from NVIDIA's repositories and check their published SHA-256.

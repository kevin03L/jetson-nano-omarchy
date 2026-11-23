# reComputer J1020 v2 (Jetson Nano 4 Go eMMC + carte Seeed J202) : kernel mainline + nvgpu + CUDA 12.9 + Omarchy

Fork de travail : https://github.com/kevin03L/jetson-nano-omarchy, branche `j202`.
Projet d'origine : https://github.com/Kristianaaron/jetson-nano-omarchy (mis au point sur un dev kit NVIDIA avec module SD).
- Kernel `linux-nano` 7.2.11 (ALARM linux-aarch64 + patches Tegra210 0101-0103)
- nvgpu/nvmap R36 portés : `Kristianaaron/linux-nvgpu` et `Kristianaaron/linux-nv-oot` (forks d'OE4T)
- Espace utilisateur NVIDIA R36.4.7 (paquets t234 Orin) pour libcuda et Vulkan
- CUDA 12.9 (dernière version pour sm_53), `nanoblas` à la place de cuBLAS. Pas de cuDNN ni TensorRT.

## Matériel
| Élément | Remarque |
|---|---|
| Module Nano 4 Go P3448-0002, 16 Go d'eMMC | le projet d'origine utilisait le module SD P3448-0000 |
| Carte porteuse Seeed J202, 9-12 V / 5 A | conçue comme la carte du Xavier NX |
| Console série 3,3 V | UART du connecteur 12 broches de la J202, 115200 8N1 |
| WSL2 (Ubuntu 24.04) + usbipd-win 5.3.0 | flash en mode recovery depuis Windows |
| SSD NVMe M.2 Key M (à prévoir) | les 16 Go d'eMMC ne suffisent pas pour les builds (environ 60 Go) |

## J202 avec un Nano : ce qu'on sait
Sources : wiki Seeed (reComputer_J2021_J202_Flash_Jetpack, reComputer_J1020v2_with_Jetson_getting_start).
- Seeed flashe avec la config NVIDIA standard `jetson-nano-emmc`, sans DTB Seeed. L'image usine (JetPack 4.6.6) ne change que la config DP et active le lecteur SD de la carte porteuse.
- Avec un Nano, seul le M.2 Key M (PCIe x1) est utilisable ; le Key E et le CAN sont réservés au Xavier.
- Mode recovery : cavalier FC REC ↔ GND carte éteinte, puis `lsusb` affiche `0955:7f21`.

## Device tree (`images/mainline-kernel/j202/`)
Le DTS mainline `tegra210-p3450-0000.dts` a été écrit pour le module **SD**. `tegra210-p3448-0002-j202.dts` l'inclut et :
1. ajoute l'eMMC `mmc@700b0600` (8 bits, non amovible, vqmmc 1,8 V, vmmc 3,3 V), absente du mainline ;
2. fixe les alias `mmc0` = eMMC et `mmc1` = microSD de la J202, pour que `root=/dev/mmcblk0p1` reste stable ;
3. désactive `mmc@700b0400` (SDIO du Key E, non câblé pour un Nano) ;
4. garde tel quel le PCIe (Ethernet RTL8111 sur le module, M.2 Key M en x1), le HDMI et le DP.
Compilé sans erreur sur le mainline v7.4-rc4. Pas encore démarré sur la carte.
À vérifier sur la carte : la broche de détection du lecteur microSD (`cd-gpios`), le ventilateur et les GPIO du 40 broches.
Attention : le fichier `tegra210-p3448-0002-p3449-0000-b00.dts` du projet d'origine n'est **pas** un DTS eMMC, malgré son nom.

## Phase 0 : préparer (flash direct, rien à sauvegarder)
Fait :
- [x] DTS et DTB J202, et `images/mainline-kernel/make-j202-dts.sh` pour l'ajouter à un arbre kernel
- [x] `scripts/flash-j202-emmc.sh` (L4T R32.7.6 + rootfs Arch → eMMC)
- [x] BSP et archive Arch (md5 vérifié) pré-téléchargés dans `/var/tmp/j202-flash/cache` (WSL)
À faire :
1. Brancher la console série.
2. Jetson éteint, cavalier FC REC ↔ GND, puis allumer.
3. PowerShell **administrateur** : `usbipd list`, `usbipd bind --busid <BUSID>`, `usbipd attach --wsl --busid <BUSID> --auto-attach`.

## Phase 1 : Arch Linux ARM sur le kernel mainline (sans CUDA)
**État au 2026-11-23 : fait.** ALARM 7.1.6 démarre depuis l'eMMC jusqu'au login (console série OK, Ethernet 1 Gb/s).
- Corrigé en route : un initramfs est obligatoire (`i2c-tegra` est un module, et le PMIC qui alimente l'eMMC est derrière) ; la QSPI est désactivée dans le DTS.
- Reste à faire : PWM (régulateur GPU, ventilateur), affichage (`Cannot find any crtc`), RAM à 204 MHz, déploiement du DTB sans QSPI sur la carte (copie dans `/boot/mainline/j202.dtb` via SSH).

- Dans WSL : `cd /mnt/c/Users/Lerou/Documents/jetson/jetson-nano-omarchy && sudo LOG=/mnt/c/Users/Lerou/Documents/jetson/wsl-live.log bash scripts/flash-j202-emmc.sh`
- Résultat : le bootloader NVIDIA R32.7.6 d'origine dans l'eMMC. `mmcblk0p1` contient Arch avec le kernel mainline d'ALARM (`/boot/mainline/Image`) et le DTB J202, sans initramfs : sdhci-tegra, ext4, pcie-tegra et nvme sont intégrés au kernel ALARM.
- Au premier boot, vérifier : la console série, le SSH (`alarm`/`alarm` ; changer aussi `root`/`root`), l'Ethernet, `lsblk` (eMMC = mmcblk0) et `lspci` (SSD).
- Limites du kernel ALARM brut :
  - la RAM reste à 204 MHz (pas de `TEGRA210_EMC`) ;
  - l'USB 3 peut manquer au premier boot (firmware xusb).
  Le kernel `linux-nano` corrige les deux en phase 2.
- Si le boot se fige pendant l'initialisation DP (problème cité par Seeed), reflasher le bootloader avec l'image Seeed `mfi_recomputer-nano-4g-4.6.6-…`, puis réécrire seulement `mmcblk0p1`.
- Ensuite, déplacer `/` sur le SSD NVMe.

## Phase 2 : la pile Nano
- Faire construire et installer le DTB J202 par le PKGBUILD `linux-nano` (via `make-j202-dts.sh`), et pointer `omarchy/nano/lib-extlinux.sh` sur ce DTB et sur la bonne partition racine.
- `bash omarchy/nano/provision-nano.sh`, puis `bash scripts/build-nano-stack.sh --dry-run`, puis sans `--dry-run`.
- Le build prend de nombreuses heures sur le Nano ; distcc vers WSL2 (cross aarch64) peut accélérer.
- Revoir le profil d'alimentation `jetson-nano-board` : il est calibré pour le 5 V du dev kit, la J202 est en 12 V.

## Phase 3 : validation
- Entrée `nano-cuda` ; `nano` sert de secours (nouveau).
- `/usr/lib/nanoblas/gemm_test` doit afficher que tous les tests passent ; vérifier aussi un `deviceQuery`.
- Problèmes connus du projet d'origine : un reset parasite sur environ un redémarrage à chaud sur trois, pas de hot-plug HDMI, l'extinction ne coupe pas l'alimentation.

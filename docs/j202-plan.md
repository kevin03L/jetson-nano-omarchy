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
- 2026-11-24 : DTB sans QSPI déployé ; `linux-firmware-nvidia` installé et `xusb.bin` mis dans l'initramfs (`FILES=`), donc USB 3 et Bluetooth Intel OK. Paquets kernel bloqués (`IgnorePkg = linux-aarch64 linux-aarch64-headers`).
- Redémarrage à chaud : se fige parfois avant même le bootloader (une fois sur deux essais), il faut couper le 12 V. Même problème que le projet d'origine, à revoir avec `linux-nano` (patch 0103).
- Reste à faire : PWM (régulateur GPU, ventilateur), affichage (`Cannot find any crtc`), RAM à 204 MHz, Ethernet parfois négocié à 100 Mb/s.

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
**État au 2026-11-24 : kernel fait.**
- SSD NVMe formaté (une partition ext4 `NANO_ROOT`), `/` dessus. `/boot` reste sur l'eMMC (U-Boot y lit `extlinux.conf`), monté via `/mnt/emmc` + bind dans `/etc/fstab`.
- `linux-nano` 7.2.11 compilé en croisé dans WSL2 : `scripts/cross-build-linux-nano.sh` (makepkg Ubuntu + gcc 14 aarch64, 30 min sur 16 cœurs). Plugins GCC coupés (`config.nano`) pour que les modules compilés sur le Nano avec gcc 16 (nvgpu-nano) se chargent. Les outils hôtes des headers sont x86 : les recompiler sur le Nano avant nvgpu-nano.
- Entrée extlinux `nano` (défaut) : `/boot/Image-nano`, `/boot/initramfs-linux-nano.img` (config `zz-jetson-nano.conf` du projet d'origine), DTB `/boot/j202/nano-j202.dtb`.
- Résultat : PWM, ventilateur, régulateur GPU et nouveau (GM20B) OK ; trois redémarrages à chaud de suite OK (patch 0103).
- RAM à 1600 MHz : cboot laisse son DTB (avec les tables EMC entraînées) en RAM à `0x83100000`. Le DTS J202 réserve cette zone ; `images/mainline-kernel/make-j202-nano-dtb.sh` copie la table du code RAM 0 dans le DTB, et `jetson-nano-emc.service` (`emc-raise`) passe à 1600 MHz au démarrage. Copie mémoire : 343 → 771 Mo/s.
- Reste : nvgpu-nano et le reste de la pile (`provision-nano.sh`, `build-nano-stack.sh`, adapter `lib-extlinux.sh` : entrée `mainline`/`nano` au lieu de `primary`, DTB J202), affichage.

- Faire construire et installer le DTB J202 par le PKGBUILD `linux-nano` (via `make-j202-dts.sh`), et pointer `omarchy/nano/lib-extlinux.sh` sur ce DTB et sur la bonne partition racine.
- `bash omarchy/nano/provision-nano.sh`, puis `bash scripts/build-nano-stack.sh --dry-run`, puis sans `--dry-run`.
- Le build prend de nombreuses heures sur le Nano ; distcc vers WSL2 (cross aarch64) peut accélérer.
- Alimentation : la J202 convertit le 12 V en 5,11 V / 8 A (schéma Seeed V1.0, page 7) pour `VDD_IN` du module ; mesuré 5,09 V par l'INA3221 du module. Avec le bloc 12 V / 5 A, mettre `SUPPLY_W=20` dans `/etc/jetson-nano/power.conf` (par défaut 10 : GPU limité à 614 MHz). La protection lit le 5 V : elle ne voit pas directement un bloc 12 V trop faible.

## Phase 3 : validation
- Entrée `nano-cuda` ; `nano` sert de secours (nouveau).
- `/usr/lib/nanoblas/gemm_test` doit afficher que tous les tests passent ; vérifier aussi un `deviceQuery`.
- Mesuré le 2026-11-24 sur `nano-cuda` (7.2.11-3) : redémarrage à chaud sans incident, nvgpu chargé, `gemm_test` 0 échec,
  Vulkan 1.3 et OpenGL 4.6 (Zink) sur le GPU, greeter SDDM et bureau Omarchy rendus par le GPU (captures relues).
  Charge 4 cœurs + GPU 240 s : 11,0 W, 5,056 V au minimum, 42 °C, GPU à 921 MHz sans réduction, 227 GFLOPS, 15 Go/s.
- Le greeter exige `omarchy/nano/nvgpu-desktop.sh` (SDDM ne transmet pas son environnement au compositeur).
- HDMI (2026-11-24) : sur la J202 aussi, `MOD_SLEEP*` (PA6) commande `VDD_3V3_HDMI` (schéma, page 13), donc le 5 V HDMI, le DDC et
  le HPD ; le noyau le met à 0. Mis à 1 par registre : HPD et EDID lus (ASUS VG249QML5A, 1920x1080 à 60 Hz), bureau et `gemm_test`
  (162 passages, 0 échec) stables 7 min. Entrée d'essai `nano-hdmi` : `regulator-always-on` sur `VDD_HUB_3V3` seul, pas encore démarrée.
- Fermeture de session (2026-11-24) : `user@1001.service` finissait en `timeout`. Cause : `bt-agent` (bluez-tools 0.2.0-6) inverse SIGTERM
  et SIGUSR1 ; il ignore l'arrêt, et Omarchy ne laisse que 5 s à `user@`. `KillSignal=SIGINT` (drop-in `bt-agent.service.d`, installé
  à l'étape 9) règle le problème : essai A/B sur la même session, échec à 5,2 s sans, arrêt propre en 0,13 s avec.
- `linux-nano` 7.2.11-4 (2026-11-24) : HDMI permanent (`regulator-always-on` sur `VDD_HUB_3V3`), avertissement AGIC (0104) et message
  Bluetooth -16 (0105) supprimés. `nano-hdmi` (7.2.11-3 + même DTB) : 35 min bureau + CUDA, 590 `gemm_test`, 0 échec. Sur 7.2.11-4,
  écran détecté aux 3 premiers redémarrages à chaud.
- Blocage GPU au démarrage (3e redémarrage de 7.2.11-4, rien de semblable aux démarrages précédents) : FECS ne répond pas à sa
  première commande, puis le pilote s'attend lui-même et `hung_task_panic` redémarre la carte 2 min plus tard. Patch nvgpu 0002 :
  plus d'auto-attente, remise à zéro de GR et 3 essais. Testé avec une panne simulée (1 échec : récupéré, CUDA OK). Cause inconnue.
  L'extinction volontaire du GPU (`echo 0 > /dev/nvgpu/igpu0/power`) plante le pilote : pas corrigé, pas utilisé.
- eMMC muette au démarrage (1 fois sur une dizaine) : HS400 échoue (`mmc_hs400_to_hs200 failed, error -110`), `/boot` non monté, mode
  secours sans réseau. Correctifs : `no-mmc-hs400` dans le DTS, `nofail` sur `/mnt/emmc` et `/boot`. Pas encore démarrés.
- Ethernet 100 Mb/s (registres du PHY, 2026-11-24) : firmware `rtl8168f-2` chargé ; les deux côtés annoncent bien 1000FD, l'apprentissage
  gigabit échoue en 1,3 s (`rxloc`/`rxrem` à 0 ne prouvent rien : ils le sont aussi avec un lien à 1 Gb/s), 3 essais, puis repli à 100. Pareil sans EEE, en maître ou esclave forcé ;
  routeur forcé à 1 Gb/s : plus de lien. La négociation n'utilise que 2 paires, le gigabit les 4 : paires 4-5 ou 7-8 (câble, prise, port).
  Même câble, autre port de la box : 1 Gb/s aussitôt (84 Mo/s par SSH). Port de la box ou contact à cet endroit.
- Problèmes connus du projet d'origine : un reset parasite sur environ un redémarrage à chaud sur trois, pas de hot-plug HDMI, l'extinction ne coupe pas l'alimentation.

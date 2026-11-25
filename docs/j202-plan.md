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

- Dans WSL, depuis le dépôt : `sudo bash scripts/flash-j202-emmc.sh`
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
- GPU : mise sous tension (2026-11-24). Le portage mainline de nvgpu avait perdu des étapes de L4T : l'horloge des fusibles (le GPU les
  lit quand sa tension monte ; sans elle, tension coupée 20 ms = le GPU ne revient jamais), la purge de ses accès mémoire, l'isolation
  (clamp) et le reset. Cause probable du blocage GPU vu une fois au démarrage (FECS muet, puis le pilote s'attendait lui-même).
  nvgpu-nano 0002 (plus d'auto-attente, 3 essais), 0003 (microcode refait après une extinction forcée), 0004 (séquence de L4T), linux-nano
  0106 (clamp). Mesuré : 3700 cycles d'alimentation sous charge, 0 échec (avant : carte figée en quelques centaines).
- eMMC muette au démarrage (2 fois sur 29) : `tegra210.dtsi` règle l'horloge de SDMMC4 (eMMC) depuis le nœud de la microSD ; si l'eMMC est
  déjà active, son horloge saute de 179 à 998 MHz (trace `clk_set_rate`). Le DTS J202 la règle depuis l'eMMC, en HS200 ; `nofail` sur
  `/mnt/emmc` et `/boot`. Ensuite 30 démarrages sans erreur.
- Veille (SC7) : linux-nano 0107 (EMC à 204 MHz avant la veille : le firmware la réveille à ce débit), 0109 (registres du SMMU restaurés,
  sinon l'écran lit des adresses brutes et le PMU du GPU finit par s'arrêter), 0108 (alimentation USB partagée), pilotes RTC pour le réveil.
  Mesuré : 15 veilles réelles de 10 s, puis `systemctl suspend` : 0 erreur GPU, écran, eMMC ; CUDA OK à chaque réveil. Réveil au clavier
  et au bouton non testés.
- 7.2.11-4 final : 10 redémarrages à chaud sur 10 (52 à 57 s). Démarrage à froid non testé sur cette version.
- Ethernet 100 Mb/s (registres du PHY, 2026-11-24) : firmware `rtl8168f-2` chargé ; les deux côtés annoncent bien 1000FD, l'apprentissage
  gigabit échoue en 1,3 s (`rxloc`/`rxrem` à 0 ne prouvent rien : ils le sont aussi avec un lien à 1 Gb/s), 3 essais, puis repli à 100. Pareil sans EEE, en maître ou esclave forcé ;
  routeur forcé à 1 Gb/s : plus de lien. La négociation n'utilise que 2 paires, le gigabit les 4 : paires 4-5 ou 7-8 (câble, prise, port).
  Même câble, autre port de la box : 1 Gb/s aussitôt (84 Mo/s par SSH). Port de la box ou contact à cet endroit.
- 7.2.11-5 et nvgpu-nano -6 (2026-11-24). Endurance : 22 cycles de charge complète (CPU, GPU, NVMe, 3D, veille de 20 s), 0 erreur, sur 7.2.11-4.
  - Fréquence du GPU bloquée après certaines veilles : une tâche mise en file sur une file gelée puis annulée laisse la file « en attente » et
    la tâche suivante ne tourne plus jamais (linux-nano 0110, files `WQ_UNBOUND | WQ_FREEZABLE`). Module de test : 2 blocages sur 3 avant, 0 sur 6 après.
  - Capteur de puissance INA3221 muet après une veille : le contrôleur I2C de la caméra est initialisé avant host1x (0111). 0 erreur après 10 veilles.
    Le limiteur de puissance recrée aussi le capteur si besoin.
  - Veille de plus de 64 s : le watchdog du PMIC continuait et redémarrait la carte (0113) ; 150 s : réveil normal (une fois).
  - Première veille après un démarrage, console série endormie : le micrologiciel (ATF) écrit `BPMP not powered on` sur l'UART A, dont l'horloge
    est coupée, et attend pour toujours. 4 blocages sur 4 ; 0114 garde l'horloge : 3 premières veilles sur 3 réussies
    (30, 20 et 120 s), console série endormie, le message sort et la carte continue.
  - CMA 256 Mo : plus d'échec d'allocation. Démarrage à froid validé (7.2.11-5, GPU 998 MHz). `nano-prev` (7.2.11-4) démarré : bureau, GPU, CUDA.
    La mise à jour de nvgpu-nano retire son module de l'ancien noyau : le garder à part et le remettre dans `extramodules` (puis `depmod`).
- Fréquences plus hautes, désactivées par défaut (entrée de démarrage séparée `nano-oc` sur la carte) : GPU en mode NA (nvgpu-nano 0006,
  `nvgpu.gpu_max_khz`), CPU (linux-nano 0112, `clk_tegra124_dfll_fcpu.max_khz` et `nano.cpu_max_khz` pour le limiteur).
  Essais courts (1 à 4 min par palier, calculs comparés un à un, 0 erreur, alimentation à 5,05 V au minimum) :
  GPU 998 MHz (1079 mV) FP32 +7,5 % ; 1075 (1121) +15,8 % ; 1152 (1163) +24,3 %, 3D +13 à +18 %.
  CPU 1555 MHz +5 % ; 1683 (1195 mV) ; 1785 (1227 mV) : stress-ng +21 % et SHA-256 +21 % sur 1479 MHz.
  CPU 1785 + GPU 1152 : 13,9 W et 42 °C au pic. Stabilité (2026-11-25, 7.2.11-5 et nvgpu -7) :
  2 h de CPU + GPU + 700 Mo de mémoire vérifiée à 1785 + 1152 : 343 tours GPU, 407 tours CPU, 34 `gemm_test`, 0 faux, 0 erreur ; 14,4 W, 44 °C,
  5,040 V au minimum, limiteur jamais actif, 216 à 228 GFLOPS.
  Veille à ces fréquences (2026-11-25) : 9 réveils normaux, calculs justes ; 12 chocs de charge CPU + GPU : 5,064 V au minimum.
- Mode de puissance (jetson-nano-board -11) : `jetson-nano-mode` (état, `list`, `set`, `menu`) et « Mode de puissance » dans le lanceur. Éco 918/537 MHz
  (ventilateur discret), normal 1479/921, performance 1683/1075, maximum 1785/1152 ; gardé au redémarrage ; sans mot de passe pour l'utilisateur
  devant l'écran (polkit, action limitée à ces quatre modes). Essayé : les 4 modes, menu au clavier, redémarrage. Sur la carte, `nano-oc` est
  l'entrée par défaut et `nano-cuda` (fréquences d'origine) le secours.
- nvgpu-nano -7 (2026-11-25, relecture des PR du projet d'origine) : microcode refait si sa projection mémoire a échoué (0003) ; vidage des
  accès mémoire raté (0004) : GPU remis à zéro, nouvel essai, sinon rail gardé avec le GPU en reset. Refuser la coupure ne marche pas : le GPU
  arrêté ne repart qu'après un reset. Pannes simulées (module d'essai, non publié) : vidage raté 1 et 2 fois, 11 mises sous tension ratées, veille
  avec vidage raté : GPU revenu à chaque fois, calculs justes. 600 cycles d'alimentation sous charge CPU : 0 échec (version précédente de -7).
- Écran de démarrage (2026-11-25). Premier logo : cboot l'affiche depuis la partition eMMC `BMP` (192 Kio, blob LZ4 `NVIDIA__BLOB__V2`) ;
  `omarchy/nano/bootlogo.py` remplace les pixels (build, install avec sauvegarde dans /root et relecture, restore). U-Boot n'écrit que sur la série.
  Chargement : Plymouth, thème `jetson-nano` (image du logo, Omarchy, barre), `tegra-drm` dans l'initramfs (écran à 5 s au lieu de 12 s), crochet
  `nano-slim` (sans hwdb ni X11 : 12,1 Mo, limite 14,7). Entrée `nano-oc` : `console=tty0` retiré, `splash plymouth.ignore-serial-consoles
  vt.global_cursor_default=0 logo.nologo` ; le noyau reste bavard sur la série. Les autres entrées gardent le texte (dépannage).
  Images à fournir dans /usr/share/plymouth/themes/jetson-nano (hors dépôt) : splash.png (1100x350), omarchy.png (400x94), bar-empty.png, bar-full.png (1x1) ;
  puis `plymouth-set-default-theme jetson-nano` et `mkinitcpio -p linux-nano`. Essayé : `nano-oc`, `nano-cuda`, `nano` (nouveau).
- Extinction réelle (2026-11-25) : `system-power-controller` sur le PMIC (ajouté par `make-j202-nano-dtb.sh`). `SFT_RST_WK` est à 0 et
  `WK_ALARM1` à 1 (registre 0x42 = 0x5f). `systemctl poweroff` coupe la carte ; l'alarme de rtc0 la rallume à l'heure dite.
- Arrêt en 1,8 s au lieu de 17 (2026-11-25) : systemd attendait 15 s la console du port micro-USB (`gs_close`) ; jetson-nano-usb-gadget-down la
  débranche avant. Redémarrage : 54 → 38 s. 12 redémarrages à chaud : 0 reset parasite (défaut du projet d'origine).
- Noyau précédent gardé (`keep-kernel`, crochets pacman) : modules, module GPU, Image, initramfs, DTB, entrée `nano-prev`. Essayé -5 → -4 → -5.
- `scripts/j202-root-to-nvme.sh` : racine de l'eMMC vers le NVMe ; essayé sur un disque virtuel seulement.
- Débit mémoire après une veille (linux-nano 7.2.11-6, 0115) : le micrologiciel remet PLLM à 1305,6 MHz, allumée, et le noyau la laisse ; mémoire
  18 % plus lente pour toujours (GPU 18,6 → 15,4 Go/s, glmark2 530 → 430). Corrigé : 18,64 Go/s après 9 veilles sur 9.
- Essai long (7.2.11-5, nvgpu -7, 1785 + 1152, 18 cycles, environ 50 min) : CPU + GPU + mémoire + NVMe vérifiés, 3D, veille : 0 erreur ; 13,9 W, 43 °C.
- Décodage vidéo matériel (2026-11-25, vaapi-tegra -1, nvidia-l4t-gm20b-firmware -2, `fix-video-dtb.sh`) : NVDEC ne répond que si le domaine NVJPG
  est alimenté ; sans cette dépendance dans le device tree, le premier accès bloque la carte (le pilote refuse alors de décoder). VIC à 408 MHz au
  lieu de 19,2. H.264 identique au logiciel (baseline, high, 16 références, tranches, entrelacé, 4K), MPEG-2 à 70 dB (arrondi). 1080p avec relecture :
  116 images/s et 0,6 s de processeur au lieu de 3,9 (180 images) ; 4K : 30 images/s. mpv s'en sert seul (`hwdec=auto-safe` d'Omarchy, vaapi-copy) :
  1080p30 64 % d'un cœur au lieu de 118 ; 1080p60 91 % au lieu de 208, avec `profile=fast` (sinon 12 images perdues par seconde, aussi en logiciel).
  Essayé : 3 flux à la fois, flux abîmé, 3 veilles en plein décodage, mémoire stable. Chromium décode toujours avec le processeur (rien de changé).
  Pas de H.265, VP9, AV1 ni 10 bits. MPEG-2 720 lignes entrelacé (inexistant en pratique) : erreurs.
- Problèmes connus du projet d'origine, pas revus ici : pas de hot-plug HDMI (à essayer).

# Outil de déploiement Windows (APX), natif, en Rust : architecture et état

Novembre 2026. Architecture complète décodée depuis le BSP L4T R32.7.6 et implémentée en Rust natif dans
[tools/j202-installer](file:///c:/Users/Lerou/Documents/jetson/jetson-nano-omarchy/tools/j202-installer/src/main.rs).

## Le problème
En recovery (cavalier FC REC sur GND), le Nano se présente en USB `0955:7f21` (« APX ») : c'est la ROM de
démarrage du Tegra210, en mode RCM. `scripts/flash-j202-emmc.sh` passe par les outils Linux x86 de NVIDIA
(`flash.sh`, `tegraflash.py`, `tegrarcm`, `tegradevflash`, `tegrabct`, `tegrahost`), d'où WSL2.

## Architecture retenue : RCM + NV3P + CBoot recovery (100 % natif Rust, sans WSL ni écriture disque Windows)
L'analyse des binaires BSP (`tegrarcm`, `tegradevflash`, `tegrabct`, `tegrahost`) a montré que l'applet
`nvtboot_recovery.bin` **et** `cboot.bin` en mode recovery parlent exactement le même protocole USB bulk **NV3P**
(`NvTegra3p*`) :

1. **RCM BootROM (`rcm.rs`)** :
   - En-tête T210 de **680 octets (`0x2a8`)**, section signée à partir de `0x238` (568 o), signature
     `zerosbk` = AES-128-CMAC clé nulle à `0x138..0x148`, padding `0x80 0x00...`.
   - `QueryVersion` (opcode `6`, 1032 o) puis `DownloadExecute` (opcode `4`, `nvtboot_recovery.bin` à `0x40010000`).
2. **NV3P Applet (`nv3p.rs`, `bct.rs`, `t210_sign.rs`)** :
   - Trame NV3P v2 : en-tête 12 o `[version=2, type, seq]` + longueur 4 o (sur Command/Data) + corps +
     checksum 4 o (`0 - sum_bytes(header_12 + body)`, longueur exclue) + `ACK (4)` / `NACK (5)`.
   - Vérification `is_applet` (`cmd 1`, `mode == 1`).
   - Envoi du BCT (`cmd 3`, tag `b"bct"`, généré en pur Rust depuis `P3448_A00_lpddr4_204Mhz_P987.cfg` avec
     `Odmdata = 0xa4000` à `0x508` et CMAC à `0x310` — identique octet par octet à `tegrabct`), de `cboot.bin`
     (`b"ebt"` à `0x92c00000`) et du DTB recovery (`b"rp1"` à `0x83000000`), tous signés `zerosbk` (`t210_sign.rs`).
   - Démarrage de CBoot (`cmd 9`, `BootMode::Recovery = 2`).
3. **NV3P CBoot Recovery (`nv3p.rs`, `install.rs`)** :
   - Vérification `is_cboot` (`cmd 1`, `mode == 2`).
   - Sauvegarde des partitions critiques (`BCT`, `PT`, `NXC`, `EBT`) via `read_partition` (`cmd 2`) dans le cache local.
   - Décompression streaming (`zstd`) de `j202-rootfs-*.img.zst` avec injection en place de `/boot/j202-firstboot.conf`
     dans le bloc marqueur de 4096 octets (`FirstbootPatchReader`), envoyée directement à `APP` via `write_partition_reader` (`cmd 3`).
   - Relecture de vérification (`APP`, signature ext4 `0xEF53`) puis `reboot(RebootMode::Coldboot)` (`cmd 6`).

## Ce qui est vérifié sur matériel et tests
- Pilote WinUSB : installation en un clic par certificat auto-signé + catalogue signé + `pnputil` (testé OK sur la carte).
- Sonde BootROM lecture seule : lecture UID 16 o (`probe.rs`, testé OK sur la carte).
- Générateur BCT pur Rust (`bct.rs`) : SHA-256 unsigned (`7c819384...`) et CMAC (`05ba4ebc...`) identiques octet par octet à `tegrabct`.
- Trames RCM (`rcm_0`, `rcm_1`), signature T210 (`t210_sign.rs`), transport NV3P (`nv3p.rs`), et patcher firstboot (`install.rs`) : couverts par tests unitaires.
- Sonde complète lecture seule (`examples/probe_cboot.rs`) : prête à être exécutée sur une carte fraîchement alimentée en recovery.

## Sécurité et conformité
- Protocole officiel NVIDIA (`zerosbk` sur module devkit non fusé) ; aucune faille bootrom exploitée.
- Aucun binaire NVIDIA redistribué dans le dépôt : `l4t.rs` extrait dynamiquement depuis l'archive `Jetson-210_Linux_R32.7.6_aarch64.tbz2` téléchargée par l'utilisateur après acceptation de la licence NVIDIA.

# Reprise — Outil de déploiement Windows pour Jetson Nano J202

Doc de référence de l'installeur natif Windows Rust (`tools/j202-installer/`).

## But
Outil Windows **natif en Rust, sans WSL**, qui installe jetson-nano-omarchy sur un reComputer J1020 v2
(Jetson Nano T210, carte porteuse J202) **par le mode recovery USB (RCM + NV3P)**, sans les outils Linux x86 de NVIDIA.

## État de l'implémentation (`tools/j202-installer/`)
- `release.rs` : liste les releases GitHub (`kevin03L/jetson-nano-omarchy`) et les dossiers locaux, détecte `j202-rootfs-*.img.zst`.
- `download.rs` : téléchargements en tâche de fond avec reprise/cache.
- `account.rs` : validation utilisateur/mot de passe/hostname + hachage SHA-512 crypt (`$6$`) pour `/boot/j202-firstboot.conf`.
- `usb.rs` : détection APX (`0955:7f21`) via `cfgmgr32` + `nusb`.
- `driver.rs` : installation WinUSB en un clic (certificat auto-signé + catalogue signé + `pnputil`, testé OK sur la carte).
- `rcm.rs` : trame RCM T210 exacte (`HEADER_LEN = 0x2a8`, `SIGN_OFFSET = 0x238`, `CMAC_OFFSET = 0x128`), timeouts explicites sur `try_read_device_id()` et `send_message()`, séquence `boot_applet()`.
- `nv3p.rs` : implémentation complète du protocole NV3P v2 (checksum `0 - sum(header_12 + body)`, émission champ par champ via `send_frame` comme `NvTegra3pSend`, gestion `ACK`/`NACK` et `CMD_STATUS` dans `data_receive`, `is_applet`, `applet_write`, `applet_boot`, `is_cboot_timeout`, `read_partition`, `read_partition_head_and_hash`, `write_partition_reader`, `erase_partition`, `reboot`).
- `t210_sign.rs` : alignement 16 o (`0x80`), mise à jour des champs taille (`0x008`, `0x240`) sur les binaires pré-entêtés (ou préfixage d'un en-tête T210 `0x400`) + CMAC `zerosbk` à `0x118`, vérifié par SHA-256 face à `tegrahost` + `tegrasign`.
- `bct.rs` : parseur pur Rust de `P3448_A00_lpddr4_204Mhz_P987.cfg` (474 champs SDRAM, `Odmdata = 0xa4000`, champs `dev_params` eMMC + `custinfo` instance `3`, CMAC à `0x0310`), vérifié octet par octet contre `tegrabct` + `tegraparser` + `tegrasign`.
- `l4t.rs` : extraction sélective en cache depuis `Jetson-210_Linux_R32.7.6_aarch64.tbz2` (ou dossier `Linux_for_Tegra/`).
- `install.rs` : pipeline complet en 7 étapes, sauvegarde `BCT`/`DTB`/`TOS`/`EKS`/`LNX`/`BMP`/`RP4`, vérification `BCT`/`DTB`, décompression streaming `zstd` avec `FirstbootPatchReader` (injection du bloc 4096 o `/boot/j202-firstboot.conf`), relecture de contrôle ext4 (`0xEF53`), et redémarrage `Coldboot`.
- `main.rs` : interface `egui`/`eframe` avec confirmation d'effacement et barre de progression en temps réel.
- `examples/probe.rs` et `examples/probe_cboot.rs` : sondes strictement lecture seule.
- `scripts/j202-make-rootfs.sh` : inclut le fichier marqueur 4096 o `# J202_FIRSTBOOT_PLACEHOLDER_V1` dans `/boot/j202-firstboot.conf`.

## Validation matérielle effectuée (lecture seule sur J202 réelle)
Commande exécutée avec succès (code de sortie `0`) :
```powershell
cargo run --manifest-path jetson-nano-omarchy/tools/j202-installer/Cargo.toml --example probe_cboot -- C:\Users\Lerou\Documents\jetson\l4t-bsp\l4t.tbz2 --reboot-recovery
```
Résultats observés sur matériel réel :
- **BootROM UID** : `4084010c0000000cc9b23d6401101032`
- **Applet `nvtboot_recovery.bin`** (`QueryVersion` + `DownloadExecute` RCM) : `version 00.01.0000`
- **NV3P Applet** : téléchargement `bct` (`10240 o`), `ebt` (`485952 o`), `rp1` (`238224 o`) + `applet_boot(Recovery)` OK
- **CBoot recovery** (`essai 0`) : `version 00.01.0000`
- **Partitions eMMC lues (lecture seule)** :
  - `BCT` : `10240 o`, SHA-256 `23896c2031a130c7610549ef9139b2ebe9d592afa2e02dc4dd80c013388501ca`
  - `DTB` : `1048576 o`, SHA-256 `a986cd527c66c879be041cc2100a149c34cb09477fe59a28e3ee58bb0e366bd0`
  - `TOS` : `6291456 o`, SHA-256 `50dc66c857a5688390c85941b87705c649cd2c6b98699856f3c26cb91bfea96d`
  - `EKS` : `81920 o`, SHA-256 `ff440665fcb88c5c7effc89f16c0f3ed602282a8f3e4b743e4bdfff3a12a6eec`
  - `LNX` : `67092480 o`, SHA-256 `1bd62126d0f00c8d8485b8fd535db23932f6c3f409e359ebd6cdbb15c6d95295`
  - `BMP` : `196608 o`, SHA-256 `bb3c6abb1f2115bb2a74bdb41a952c66d6d7087dce0703e5b280dac49934e48b`
  - `RP4` : `262144 o`, SHA-256 `96d37f240db3dbde6df652f25ecd29b24e7c8ef7a70da240c521b2fd359c288c`
- **Reboot recovery** (`RebootMode::Recovery`) : retour automatique en BootROM APX sans coupure 12 V vérifié.

## Seule étape restante avant le premier déploiement réel
1. Fabriquer une image `j202-rootfs-*.img.zst` sur hôte `aarch64` avec `scripts/j202-make-rootfs.sh` (incluant le slot `/boot/j202-firstboot.conf`), puis valider l'écriture de `APP` et le premier démarrage complet via `j202-installer.exe`.

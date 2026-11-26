//! Extracts the few NVIDIA binaries and BCT config needed for RCM + CBoot recovery
//! from the user-downloaded `Jetson-210_Linux_R32.7.6_aarch64.tbz2` archive (or an already
//! extracted `Linux_for_Tegra` directory) into the local cache. No NVIDIA binary is bundled.

use crate::bct;
use crate::t210_sign;
use bzip2::read::BzDecoder;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

const REL_NVTBOOT_RECOVERY: &str = "Linux_for_Tegra/bootloader/nvtboot_recovery.bin";
const REL_CBOOT: &str = "Linux_for_Tegra/bootloader/t210ref/cboot.bin";
const REL_BCT_CFG: &str = "Linux_for_Tegra/bootloader/t210ref/BCT/P3448_A00_lpddr4_204Mhz_P987.cfg";
const REL_DTB: &str = "Linux_for_Tegra/kernel/dtb/tegra210-p3448-0002-p3449-0000-b00.dtb";

const NEEDED: [&str; 4] = [REL_NVTBOOT_RECOVERY, REL_CBOOT, REL_BCT_CFG, REL_DTB];

pub struct RecoveryPayloads {
    pub nvtboot_recovery: Vec<u8>,
    pub signed_bct: Vec<u8>,
    pub signed_ebt: Vec<u8>,
    pub signed_rp1: Vec<u8>,
}

/// Ensures the four required files exist under `cache/l4t-r32.7.6/` (or reads from an adjacent
/// `Linux_for_Tegra/` directory if `l4t_path` is already extracted), then prepares the signed
/// BCT, CBoot (`EBT`), and recovery DTB (`RP1`).
pub fn load_recovery_payloads(l4t_archive_or_dir: &Path, cache_dir: &Path) -> Result<RecoveryPayloads, String> {
    let root = if l4t_archive_or_dir.is_dir() && l4t_archive_or_dir.join("bootloader").is_dir() {
        l4t_archive_or_dir.to_path_buf()
    } else {
        let extracted = cache_dir.join("l4t-r32.7.6");
        ensure_extracted(l4t_archive_or_dir, &extracted)?;
        extracted.join("Linux_for_Tegra")
    };

    let nvtboot_recovery = read_rel(&root, REL_NVTBOOT_RECOVERY)?;
    let cboot_raw = read_rel(&root, REL_CBOOT)?;
    let bct_cfg =
        String::from_utf8(read_rel(&root, REL_BCT_CFG)?).map_err(|e| format!("BCT cfg UTF-8 invalide : {e}"))?;
    let dtb_raw = read_rel(&root, REL_DTB)?;

    let signed_bct = bct::from_cfg(&bct_cfg, bct::DEFAULT_ODMDATA)?;
    let signed_ebt = t210_sign::sign_t210_image(b"EBT\0", &cboot_raw);
    let signed_rp1 = t210_sign::sign_t210_image(b"DTB\0", &dtb_raw);

    Ok(RecoveryPayloads {
        nvtboot_recovery,
        signed_bct,
        signed_ebt,
        signed_rp1,
    })
}

fn read_rel(linux_for_tegra_dir: &Path, archive_rel: &str) -> Result<Vec<u8>, String> {
    let rel = archive_rel.strip_prefix("Linux_for_Tegra/").unwrap_or(archive_rel);
    let path = linux_for_tegra_dir.join(rel);
    fs::read(&path).map_err(|e| format!("lecture de {} : {e}", path.display()))
}

fn ensure_extracted(tbz2_path: &Path, out_dir: &Path) -> Result<(), String> {
    let all_present = NEEDED.iter().all(|rel| out_dir.join(rel).is_file());
    if all_present {
        return Ok(());
    }
    if !tbz2_path.is_file() {
        return Err(format!("archive L4T introuvable : {}", tbz2_path.display()));
    }

    let file = File::open(tbz2_path).map_err(|e| format!("{} : {e}", tbz2_path.display()))?;
    let bz = BzDecoder::new(std::io::BufReader::new(file));
    let mut archive = Archive::new(bz);
    let mut found = 0usize;

    let entries = archive
        .entries()
        .map_err(|e| format!("lecture de l'archive L4T : {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("entrée tar L4T : {e}"))?;
        let path_cow = entry.path().map_err(|e| format!("chemin tar L4T : {e}"))?;
        let path_str = path_cow.to_string_lossy().replace('\\', "/");
        if NEEDED.contains(&path_str.as_str()) {
            let dest = out_dir.join(&path_str);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{} : {e}", parent.display()))?;
            }
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("extraction de {path_str} : {e}"))?;
            fs::write(&dest, &buf).map_err(|e| format!("écriture de {} : {e}", dest.display()))?;
            found += 1;
            if found == NEEDED.len() {
                break;
            }
        }
    }

    let missing: Vec<&str> = NEEDED
        .iter()
        .copied()
        .filter(|rel| !out_dir.join(rel).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "fichiers NVIDIA manquants dans l'archive L4T : {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

/// Resolves the path to an extracted or cached L4T tree for CLI examples.
#[allow(dead_code)]
pub fn default_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "j202-installer")
        .map(|p| p.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("j202-installer"))
}

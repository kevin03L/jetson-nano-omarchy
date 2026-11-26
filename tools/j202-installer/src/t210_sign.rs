//! Tegra210 image header framing and zero-key AES-128-CMAC signing (`zerosbk`),
//! matching `tegrahost --align` / `--appendsigheader` / `--updatesigheader`.

use crate::rcm::zero_key_cmac;

const BL_HEADER_LEN: usize = 0x400;
const SIGN_DATA_OFFSET: usize = 0x230;
const CMAC_OFFSET: usize = 0x118;
const MAGIC_SENTINEL: u64 = 0xfeed_dead_beef_bab1;

/// Pads `data` with `[0x80, 0x00, ...]` up to a 16-byte boundary, as `tegrahost --chip 0x21 --align` does.
pub fn align_16_pad80(data: &[u8]) -> Vec<u8> {
    let rem = data.len() % 16;
    if rem == 0 {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + (16 - rem));
    out.extend_from_slice(data);
    out.push(0x80);
    out.resize(data.len() + (16 - rem), 0);
    out
}

/// Checks if `raw` already begins with a T210 bootloader sigheader (`0x004..0x008 == magic` and
/// `0x230..0x240 == 0xcc` repeated or `0x018..0x118 == 0xaa` repeated).
fn has_bl_header(raw: &[u8], magic: &[u8; 4]) -> bool {
    raw.len() >= 0x258 && raw[0x004..0x008] == magic[..] && raw[0x018..0x028].iter().all(|&b| b == 0xaa)
}

/// Prepares and zero-SBK signs a T210 image (`EBT`, `TBC`, `DTB`, etc.) for NV3P download.
///
/// - If `raw` already has NVIDIA's 0x258-byte bootloader header (like `cboot.bin` or
///   `nvtboot_recovery_cpu.bin`), updates its zero-SBK CMAC in place at `0x118..0x128`.
/// - Otherwise (like a raw `.dtb`), aligns to 16 bytes with `0x80` padding, prepends the 0x400-byte
///   T210 header (`NvTegraHostPrependBLHdr`), and writes the zero-SBK CMAC at `0x118..0x128`.
pub fn sign_t210_image(magic: &[u8; 4], raw: &[u8]) -> Vec<u8> {
    let mut buf = if has_bl_header(raw, magic) {
        let mut out = align_16_pad80(raw);
        let total = out.len() as u64;
        out[0x008..0x010].copy_from_slice(&total.to_le_bytes());
        out[0x240..0x248].copy_from_slice(&total.to_le_bytes());
        out
    } else {
        let aligned = align_16_pad80(raw);
        let total = BL_HEADER_LEN + aligned.len();
        let mut out = vec![0u8; total];
        out[0x004..0x008].copy_from_slice(magic);
        out[0x008..0x010].copy_from_slice(&(total as u64).to_le_bytes());
        out[0x010..0x018].copy_from_slice(&MAGIC_SENTINEL.to_le_bytes());
        out[0x018..0x118].fill(0xaa);
        out[0x118..0x228].fill(0xee);
        out[0x230..0x240].fill(0xcc);
        out[0x240..0x248].copy_from_slice(&(total as u64).to_le_bytes());
        out[0x248..0x250].copy_from_slice(&MAGIC_SENTINEL.to_le_bytes());
        out[0x250..0x258].copy_from_slice(&MAGIC_SENTINEL.to_le_bytes());
        out[BL_HEADER_LEN..].copy_from_slice(&aligned);
        out
    };

    let mac = zero_key_cmac(&buf[SIGN_DATA_OFFSET..]);
    buf[CMAC_OFFSET..CMAC_OFFSET + 16].copy_from_slice(&mac);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_pad80_adds_80_then_zeros() {
        let aligned = align_16_pad80(&[1, 2, 3, 4]);
        assert_eq!(aligned.len(), 16);
        assert_eq!(&aligned[..5], &[1, 2, 3, 4, 0x80]);
        assert!(aligned[5..].iter().all(|&b| b == 0));
        assert_eq!(align_16_pad80(&[0u8; 16]).len(), 16);
    }

    #[test]
    fn dtb_header_layout_matches_tegrahost() {
        let fake_dtb = vec![0xd0u8, 0x0d, 0xfe, 0xed, 1, 2, 3, 4];
        let signed = sign_t210_image(b"DTB\0", &fake_dtb);
        assert_eq!(signed.len(), BL_HEADER_LEN + 16);
        assert_eq!(&signed[0x004..0x008], b"DTB\0");
        assert_eq!(
            u64::from_le_bytes(signed[0x008..0x010].try_into().unwrap()) as usize,
            signed.len()
        );
        assert_eq!(
            u64::from_le_bytes(signed[0x240..0x248].try_into().unwrap()) as usize,
            signed.len()
        );
        assert!(signed[0x018..0x118].iter().all(|&b| b == 0xaa));
        assert!(signed[0x128..0x228].iter().all(|&b| b == 0xee));
        assert!(signed[0x230..0x240].iter().all(|&b| b == 0xcc));
        assert_eq!(
            &signed[CMAC_OFFSET..CMAC_OFFSET + 16],
            &zero_key_cmac(&signed[SIGN_DATA_OFFSET..])
        );
    }

    #[test]
    fn preheadered_ebt_keeps_size_and_updates_cmac() {
        let mut fake_ebt = vec![0u8; 0x300];
        fake_ebt[0x004..0x008].copy_from_slice(b"EBT\0");
        fake_ebt[0x008..0x010].fill(0x11);
        fake_ebt[0x018..0x118].fill(0xaa);
        fake_ebt[0x118..0x228].fill(0xee);
        fake_ebt[0x230..0x240].fill(0xcc);
        fake_ebt[0x240..0x248].fill(0xee);
        fake_ebt[0x258..0x300].fill(0x55);
        let signed = sign_t210_image(b"EBT\0", &fake_ebt);
        assert_eq!(signed.len(), 0x300);
        assert_eq!(u64::from_le_bytes(signed[0x008..0x010].try_into().unwrap()), 0x300);
        assert_eq!(u64::from_le_bytes(signed[0x240..0x248].try_into().unwrap()), 0x300);
        assert_eq!(
            &signed[CMAC_OFFSET..CMAC_OFFSET + 16],
            &zero_key_cmac(&signed[SIGN_DATA_OFFSET..])
        );
    }

    #[test]
    fn matches_l4t_cboot_and_dtb_byte_for_byte_if_present() {
        use sha2::{Digest, Sha256};

        let cboot_path = std::path::Path::new(
            r"C:\Users\Lerou\Documents\jetson\l4t-bsp\Linux_for_Tegra\bootloader\t210ref\cboot.bin",
        );
        if cboot_path.is_file() {
            let raw = std::fs::read(cboot_path).unwrap();
            let signed = sign_t210_image(b"EBT\0", &raw);
            let hash: String = Sha256::digest(&signed).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hash, "9a0a80e58da44538d35a7aa15334f6e8ba9eae97b4a37899de7716a437194f22");
        }

        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let dtb_path = std::path::PathBuf::from(local_app_data).join(
                r"j202-installer\cache\l4t-r32.7.6\Linux_for_Tegra\kernel\dtb\tegra210-p3448-0002-p3449-0000-b00.dtb",
            );
            if dtb_path.is_file() {
                let raw = std::fs::read(dtb_path).unwrap();
                let signed = sign_t210_image(b"DTB\0", &raw);
                let hash: String = Sha256::digest(&signed).iter().map(|b| format!("{b:02x}")).collect();
                assert_eq!(hash, "af297f9c8e191dff7f50dcd686f3d8191f9d7fa00b8386c0ba25fea79fffc075");
            }
        }
    }
}

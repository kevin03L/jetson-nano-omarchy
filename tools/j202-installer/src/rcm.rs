//! The Tegra210 boot ROM recovery protocol (RCM), NVIDIA's own, as `tegrarcm`/`flash.sh` use it.
//! This is NOT the CVE-2018-6242 bootrom exploit: the Jetson Nano developer module is not fused, so
//! the boot ROM accepts a zero-SBK AES-128-CMAC message (`zerosbk`), which is exactly how the
//! official flasher talks to it. No security control is bypassed.

use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{Device, Interface, MaybeFuture};
use std::time::Duration;

/// T210 RCM message header length (0x2a8 = 680 bytes), verified from `NvTegraT21xRcmCreateMsgFromBuffer`.
pub const HEADER_LEN: usize = 0x2a8;
/// Start of the signed/hashed region (`len_secure` onwards, 568 = 0x238 bytes into the message).
pub const SIGN_OFFSET: usize = 0x238;
/// Where the 16-byte AES-128-CMAC (over `msg[SIGN_OFFSET..]`) sits in the header.
pub const CMAC_OFFSET: usize = 0x128;
/// T210 RCM version identifier stored at `0x264`.
pub const RCM_VERSION: u32 = 0x0021_0001;
/// Minimum total RCM message length enforced by the boot ROM (1032 bytes = 0x408).
pub const MIN_MSG_LEN: usize = 0x408;
/// Default T210 IRAM load/entry address for `nvtboot_recovery.bin`.
pub const T210_APPLET_ADDR: u32 = 0x4001_0000;

pub const OPCODE_DOWNLOAD_EXECUTE: u32 = 0x04;
pub const OPCODE_QUERY_VERSION: u32 = 0x06;

const EP_IN: u8 = 0x81;
const EP_OUT: u8 = 0x01;

pub struct Rcm {
    interface: Interface,
}

impl Rcm {
    pub fn open(device: &Device) -> Result<Self, String> {
        let interface = device
            .claim_interface(0)
            .wait()
            .map_err(|e| format!("accès à l'interface USB de la carte : {e}"))?;
        Ok(Self { interface })
    }

    /// Wraps an already-claimed USB interface in an RCM handle.
    pub fn from_interface(interface: Interface) -> Self {
        Self { interface }
    }

    /// Consumes the RCM handle and yields the claimed USB interface for NV3P communication
    /// with the loaded applet (`nvtboot_recovery.bin`).
    pub fn into_interface(self) -> Interface {
        self.interface
    }

    /// Clears any leftover USB STALL condition on the bulk endpoints.
    pub fn clear_halt(&self) {
        if let Ok(mut ep) = self.interface.endpoint::<Bulk, Out>(EP_OUT) {
            let _ = ep.clear_halt().wait();
        }
        if let Ok(mut ep) = self.interface.endpoint::<Bulk, In>(EP_IN) {
            let _ = ep.clear_halt().wait();
        }
    }

    /// The board's 16-byte unique id. The boot ROM returns it only on the first read after
    /// entering recovery; if already read in this session, the read times out and we return `Ok(None)`
    /// (matching `tegrarcm --skipuid` / `NvTegraRcmReadChipUid`).
    pub fn try_read_device_id(&self) -> Result<Option<[u8; 16]>, String> {
        let mut ep = self
            .interface
            .endpoint::<Bulk, In>(EP_IN)
            .map_err(|e| format!("point d'accès USB en lecture : {e}"))?;
        let buf = Buffer::new(512);
        match ep.transfer_blocking(buf, Duration::from_millis(500)).into_result() {
            Ok(data) if data.len() >= 16 => {
                let mut id = [0u8; 16];
                id.copy_from_slice(&data[..16]);
                Ok(Some(id))
            }
            Ok(data) => Err(format!("identifiant boot ROM tronqué ({} octets reçus)", data.len())),
            Err(TransferError::Cancelled) => Ok(None),
            Err(e) => Err(format!("lecture de l'identifiant de la carte : {e}")),
        }
    }

    /// The board's 16-byte unique id. Errors if it was already consumed or timed out.
    pub fn read_device_id(&self) -> Result<[u8; 16], String> {
        self.try_read_device_id()?
            .ok_or_else(|| "l'identifiant boot ROM a déjà été lu dans cette session recovery".into())
    }

    /// Sends one framed RCM message and reads the 4-byte boot ROM status/response word.
    /// Sends a zero-length USB packet when the message length is `<= 65535` and a multiple of 512,
    /// matching `NvTegraRcmInitBootRomCommunication`.
    pub fn send_message(&self, msg: &[u8]) -> Result<u32, String> {
        let mut ep_out = self
            .interface
            .endpoint::<Bulk, Out>(EP_OUT)
            .map_err(|e| format!("point d'accès USB en écriture : {e}"))?;
        for chunk in msg.chunks(0x10_0000) {
            let mut buf = Buffer::new(chunk.len());
            buf.extend_from_slice(chunk);
            ep_out
                .transfer_blocking(buf, Duration::from_secs(10))
                .into_result()
                .map_err(|e| format!("envoi RCM ({} o) : {e}", msg.len()))?;
        }

        if msg.len() <= 0xffff && msg.len().is_multiple_of(512) {
            ep_out
                .transfer_blocking(Buffer::new(0), Duration::from_secs(5))
                .into_result()
                .map_err(|e| format!("envoi ZLP RCM : {e}"))?;
        }
        drop(ep_out);

        let mut ep_in = self
            .interface
            .endpoint::<Bulk, In>(EP_IN)
            .map_err(|e| format!("point d'accès USB en lecture : {e}"))?;
        let resp = ep_in
            .transfer_blocking(Buffer::new(512), Duration::from_secs(5))
            .into_result()
            .map_err(|e| format!("réponse boot ROM RCM : {e}"))?;
        if resp.len() < 4 {
            return Err(format!("réponse boot ROM RCM tronquée ({} o)", resp.len()));
        }
        Ok(u32::from_le_bytes(resp[..4].try_into().unwrap()))
    }

    /// Executes the official two-step T210 RCM handshake:
    /// 1. `QueryVersion` (`rcm_0`), asserting the boot ROM returns `0x00210001`.
    /// 2. `DownloadExecute` (`rcm_1`) loading `applet` (`nvtboot_recovery.bin`) at `0x40010000`,
    ///    asserting the boot ROM returns `0`, then waiting 1 second for DRAM/applet init.
    pub fn boot_applet(&self, applet: &[u8]) -> Result<(), String> {
        let q = build_query_version();
        let ver = self.send_message(&q)?;
        if ver != RCM_VERSION {
            return Err(format!(
                "version boot ROM inattendue : 0x{ver:08x} (attendu 0x{RCM_VERSION:08x})"
            ));
        }

        let dl = build_download_applet(applet);
        let rc = self.send_message(&dl)?;
        if rc != 0 {
            return Err(format!("le boot ROM a rejeté l'applet RCM (code 0x{rc:08x})"));
        }

        std::thread::sleep(Duration::from_secs(1));
        Ok(())
    }
}

/// Computes the zero-key AES-128-CMAC expected by an unfused T210 module.
pub fn zero_key_cmac(data: &[u8]) -> [u8; 16] {
    use cmac::{Cmac, KeyInit, Mac};
    let signed_len = (data.len() / 16) * 16;
    let mut mac = <Cmac<aes::Aes128>>::new_from_slice(&[0u8; 16]).expect("16-byte key");
    mac.update(&data[..signed_len]);
    mac.finalize().into_bytes().into()
}

/// Builds the T210 `QueryVersion` RCM message (`rcm_0`, 1032 bytes).
pub fn build_query_version() -> Vec<u8> {
    build_message(OPCODE_QUERY_VERSION, &[], &[])
}

/// Builds the T210 `DownloadExecute` RCM message (`rcm_1`) for `nvtboot_recovery.bin`.
pub fn build_download_applet(applet: &[u8]) -> Vec<u8> {
    let mut args = [0u8; 48];
    args[0..4].copy_from_slice(&T210_APPLET_ADDR.to_le_bytes());
    build_message(OPCODE_DOWNLOAD_EXECUTE, &args, applet)
}

/// Frames a T210 RCM message identically to `NvTegraT21xRcmCreateMsgFromBuffer` and signs it with
/// zero-key AES-128-CMAC over `[SIGN_OFFSET..total]`.
pub fn build_message(opcode: u32, args: &[u8], payload: &[u8]) -> Vec<u8> {
    let base = HEADER_LEN + payload.len();
    let (total, pad_len) = if base <= 0x3ff {
        (MIN_MSG_LEN, MIN_MSG_LEN - base)
    } else {
        let p = 16 - (payload.len() & 0xf);
        (base + p, p)
    };

    let mut msg = vec![0u8; total];
    // len_insecure at 0x000
    msg[0..4].copy_from_slice(&(total as u32).to_le_bytes());

    // Signed section begins at 0x238 (SIGN_OFFSET); for zerosbk, 0x238..0x258 remain 0.
    // opcode at 0x258
    msg[0x258..0x25c].copy_from_slice(&opcode.to_le_bytes());
    // len_secure at 0x25c
    msg[0x25c..0x260].copy_from_slice(&(total as u32).to_le_bytes());
    // payload_len at 0x260
    msg[0x260..0x264].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    // rcm_version at 0x264
    msg[0x264..0x268].copy_from_slice(&RCM_VERSION.to_le_bytes());

    // args at 0x268..0x298 (48 bytes max)
    let arg_len = args.len().min(48);
    msg[0x268..0x268 + arg_len].copy_from_slice(&args[..arg_len]);

    // padding1 (8 bytes) at 0x2a0: 0x80 followed by zeros
    msg[0x2a0] = 0x80;

    // payload at 0x2a8 (HEADER_LEN)
    if !payload.is_empty() {
        msg[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    }

    // padding2 at 0x2a8 + payload.len()
    if pad_len > 0 {
        msg[HEADER_LEN + payload.len()] = 0x80;
    }

    // Zero-key AES-128-CMAC of msg[0x238..total] written to 0x128..0x138
    let mac = zero_key_cmac(&msg[SIGN_OFFSET..total]);
    msg[CMAC_OFFSET..CMAC_OFFSET + 16].copy_from_slice(&mac);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn query_version_matches_nvidia_rcm0_byte_for_byte() {
        let q = build_query_version();
        assert_eq!(q.len(), 1032);
        assert_eq!(u32::from_le_bytes(q[0..4].try_into().unwrap()), 1032);
        // Verified CMAC from tegrarcm --updatesig on rcm_0_encrypt.rcm (at 0x128..0x138)
        assert_eq!(CMAC_OFFSET, 0x128);
        assert_eq!(hex(&q[0x128..0x138]), "09685333e8b5fb98587a85a5bc130987");
        assert!(q[0x138..0x148].iter().all(|&b| b == 0));
        assert_eq!(u32::from_le_bytes(q[0x258..0x25c].try_into().unwrap()), 6);
        assert_eq!(u32::from_le_bytes(q[0x25c..0x260].try_into().unwrap()), 1032);
        assert_eq!(u32::from_le_bytes(q[0x260..0x264].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(q[0x264..0x268].try_into().unwrap()), 0x0021_0001);
        assert_eq!(q[0x2a0], 0x80);
        assert_eq!(q[0x2a8], 0x80);
        assert!(q[0x2a9..].iter().all(|&b| b == 0));
    }

    #[test]
    fn download_applet_geometry_matches_nvidia_rcm1() {
        // nvtboot_recovery.bin in L4T R32.7.6 is 185708 bytes -> total must be 186392 (0x2d818)
        let fake_applet = vec![0x14u8; 185_708];
        let msg = build_download_applet(&fake_applet);
        assert_eq!(msg.len(), 186_392);
        assert_eq!(u32::from_le_bytes(msg[0..4].try_into().unwrap()), 186_392);
        assert_eq!(u32::from_le_bytes(msg[0x258..0x25c].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(msg[0x260..0x264].try_into().unwrap()), 185_708);
        assert_eq!(
            u32::from_le_bytes(msg[0x268..0x26c].try_into().unwrap()),
            T210_APPLET_ADDR
        );
        assert_eq!(msg[0x2a0], 0x80);
        assert_eq!(msg[HEADER_LEN + 185_708], 0x80);
        assert_eq!((msg.len() - SIGN_OFFSET) % 16, 0);
    }

    #[test]
    fn zero_key_cmac_matches_known_vectors() {
        assert_eq!(hex(&zero_key_cmac(&[])), "4387c14b46ef7e176dceefa862d72ff9");
        assert_eq!(hex(&zero_key_cmac(&[0u8; 16])), "763cbcde81df9131bf897712c088edad");
    }
}

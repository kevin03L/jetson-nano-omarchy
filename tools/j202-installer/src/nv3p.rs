//! NVIDIA 3P (NV3P) USB bulk protocol spoken identically by the recovery applet
//! (`nvtboot_recovery.bin`) and CBoot in recovery mode (`cboot.bin`), verified from
//! `tegrarcm` and `tegradevflash` (`NvTegra3p*`).

use nusb::Interface;
use nusb::transfer::{Buffer, Bulk, In, Out};
use std::io::Read;
use std::time::Duration;

pub const VERSION: u32 = 2;

pub const PKT_COMMAND: u32 = 1;
pub const PKT_DATA: u32 = 2;
pub const PKT_ACK: u32 = 4;
pub const PKT_NACK: u32 = 5;

pub const CMD_IS_APPLET_OR_CBOOT: u32 = 1;
pub const CMD_READ: u32 = 2;
pub const CMD_WRITE: u32 = 3;
pub const CMD_ERASE: u32 = 4;
pub const CMD_REBOOT: u32 = 6;
pub const CMD_STATUS: u32 = 7;
pub const CMD_COMPLETE: u32 = 8;
pub const CMD_BOOT: u32 = 9;

/// Maximum data payload per NV3P data packet (1 MiB, matching `NvTegraRcmDataSend` / `NvTegra3pSendFile`).
pub const MAX_CHUNK: usize = 0x10_0000;

const EP_IN: u8 = 0x81;
const EP_OUT: u8 = 0x01;

const SHORT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootMode {
    Rcm = 1,
    Recovery = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebootMode {
    Coldboot = 0,
    Recovery = 1,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Command { seq: u32, cmd: u32, payload: Vec<u8> },
    Data { seq: u32, payload: Vec<u8> },
    Ack { seq: u32 },
    Nack { seq: u32, code: u32 },
}

/// NV3P byte-sum two's-complement checksum over the 12-byte header and the body
/// (excluding the 4-byte `len` field in Command/Data packets, per `NvTegra3pSend`/`NvTegra3pReceive`).
pub fn compute_checksum(header_12: &[u8; 12], body: &[u8]) -> u32 {
    let mut sum = 0u32;
    for &b in header_12 {
        sum = sum.wrapping_add(b as u32);
    }
    for &b in body {
        sum = sum.wrapping_add(b as u32);
    }
    0u32.wrapping_sub(sum)
}

/// Encodes a complete NV3P wire frame (header + optional len + body + checksum).
pub fn encode_packet(pkt_type: u32, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(&VERSION.to_le_bytes());
    header[4..8].copy_from_slice(&pkt_type.to_le_bytes());
    header[8..12].copy_from_slice(&seq.to_le_bytes());
    let csum = compute_checksum(&header, body);
    let has_len = pkt_type != PKT_ACK && pkt_type != PKT_NACK;
    let mut out = Vec::with_capacity(12 + if has_len { 4 } else { 0 } + body.len() + 4);
    out.extend_from_slice(&header);
    if has_len {
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    }
    out.extend_from_slice(body);
    out.extend_from_slice(&csum.to_le_bytes());
    out
}

pub struct Nv3p {
    interface: Interface,
    tx_seq: u32,
    rx_seq: u32,
    rx_buf: Vec<u8>,
}

impl Nv3p {
    pub fn new(interface: Interface) -> Self {
        Self {
            interface,
            tx_seq: 0,
            rx_seq: 0,
            rx_buf: Vec::new(),
        }
    }

    pub fn into_interface(self) -> Interface {
        self.interface
    }

    fn usb_write(&self, data: &[u8], timeout: Duration) -> Result<(), String> {
        let mut ep = self
            .interface
            .endpoint::<Bulk, Out>(EP_OUT)
            .map_err(|e| format!("endpoint USB OUT : {e}"))?;
        // Send in <= 1 MiB + 64 chunks so WinUSB transfer size stays bounded.
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + MAX_CHUNK + 1024).min(data.len());
            let mut buf = Buffer::new(end - offset);
            buf.extend_from_slice(&data[offset..end]);
            ep.transfer_blocking(buf, timeout)
                .into_result()
                .map_err(|e| format!("écriture USB NV3P : {e}"))?;
            offset = end;
        }
        Ok(())
    }

    fn usb_read_exact(&mut self, len: usize, timeout: Duration) -> Result<Vec<u8>, String> {
        if self.rx_buf.len() < len {
            let mut ep = self
                .interface
                .endpoint::<Bulk, In>(EP_IN)
                .map_err(|e| format!("endpoint USB IN : {e}"))?;
            while self.rx_buf.len() < len {
                let want = (len - self.rx_buf.len()).clamp(512, MAX_CHUNK + 4096);
                // WinUSB Bulk IN reads must be a multiple of max_packet_size (512).
                let aligned = want.div_ceil(512) * 512;
                let buf = Buffer::new(aligned);
                let res = ep
                    .transfer_blocking(buf, timeout)
                    .into_result()
                    .map_err(|e| format!("lecture USB NV3P : {e}"))?;
                if res.is_empty() {
                    return Err("lecture USB NV3P : paquet vide inattendu".into());
                }
                self.rx_buf.extend_from_slice(&res);
            }
        }
        Ok(self.rx_buf.drain(..len).collect())
    }

    fn read_u32_le(&mut self, timeout: Duration) -> Result<u32, String> {
        let b = self.usb_read_exact(4, timeout)?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn send_frame(&self, pkt_type: u32, seq: u32, body: &[u8], timeout: Duration) -> Result<(), String> {
        let mut header = [0u8; 12];
        header[0..4].copy_from_slice(&VERSION.to_le_bytes());
        header[4..8].copy_from_slice(&pkt_type.to_le_bytes());
        header[8..12].copy_from_slice(&seq.to_le_bytes());
        let csum = compute_checksum(&header, body);

        // NvTegra3pSend writes header (12 B), length (4 B, Command/Data), body, and checksum (4 B)
        // as distinct USB bulk transfers; CBoot posts separate USB reads per field.
        self.usb_write(&header, timeout)?;
        if pkt_type != PKT_ACK && pkt_type != PKT_NACK {
            self.usb_write(&(body.len() as u32).to_le_bytes(), timeout)?;
        }
        if !body.is_empty() {
            self.usb_write(body, timeout)?;
        }
        self.usb_write(&csum.to_le_bytes(), timeout)
    }

    fn send_ack(&self, seq: u32) -> Result<(), String> {
        self.send_frame(PKT_ACK, seq, &[], SHORT_TIMEOUT)
    }

    fn recv_packet(&mut self, timeout: Duration) -> Result<Packet, String> {
        let hdr_vec = self.usb_read_exact(12, timeout)?;
        let header: [u8; 12] = hdr_vec.try_into().unwrap();
        let version = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let pkt_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let seq = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if version != VERSION {
            return Err(format!("version NV3P invalide : {version} (attendu {VERSION})"));
        }
        self.rx_seq = seq;

        match pkt_type {
            PKT_NACK => {
                let code = self.read_u32_le(timeout)?;
                let _ = self.read_u32_le(timeout)?;
                Ok(Packet::Nack { seq, code })
            }
            PKT_ACK => {
                let expected = self.tx_seq.wrapping_sub(1);
                if seq != expected {
                    return Err(format!("ACK NV3P hors séquence : {seq} (attendu {expected})"));
                }
                let footer = self.read_u32_le(timeout)?;
                let sum = compute_checksum(&header, &[]);
                if footer != sum {
                    return Err("somme de contrôle ACK NV3P invalide".into());
                }
                Ok(Packet::Ack { seq })
            }
            PKT_COMMAND | PKT_DATA => {
                let body_len = self.read_u32_le(timeout)? as usize;
                let body = if body_len > 0 {
                    self.usb_read_exact(body_len, timeout)?
                } else {
                    Vec::new()
                };
                let footer = self.read_u32_le(timeout)?;
                let expected_footer = compute_checksum(&header, &body);
                if footer != expected_footer {
                    return Err("somme de contrôle paquet NV3P invalide".into());
                }
                if pkt_type != PKT_ACK {
                    self.send_ack(seq)?;
                }
                if pkt_type == PKT_COMMAND {
                    if body.len() < 4 {
                        return Err("paquet commande NV3P tronqué".into());
                    }
                    let cmd = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    Ok(Packet::Command {
                        seq,
                        cmd,
                        payload: body[4..].to_vec(),
                    })
                } else {
                    Ok(Packet::Data { seq, payload: body })
                }
            }
            other => Err(format!("type de paquet NV3P inconnu : {other}")),
        }
    }

    fn send_packet(&mut self, pkt_type: u32, body: &[u8], timeout: Duration) -> Result<(), String> {
        let seq = if pkt_type == PKT_ACK || pkt_type == PKT_NACK {
            self.rx_seq
        } else {
            self.tx_seq
        };
        self.send_frame(pkt_type, seq, body, timeout)?;
        if pkt_type != PKT_ACK && pkt_type != PKT_NACK {
            self.tx_seq = self.tx_seq.wrapping_add(1);
            match self.recv_packet(timeout)? {
                Packet::Ack { .. } => Ok(()),
                Packet::Nack { code, .. } => Err(format!("NACK NV3P reçu (code 0x{code:08x})")),
                other => Err(format!("réponse inattendue à la place d'un ACK : {other:?}")),
            }
        } else {
            Ok(())
        }
    }

    fn command_complete(&mut self) -> Result<(), String> {
        self.command_send_raw(CMD_COMPLETE, &[0u8; 4], SHORT_TIMEOUT)
            .map(|_| ())
    }

    fn command_send_raw(&mut self, cmd: u32, args: &[u8], timeout: Duration) -> Result<Option<u32>, String> {
        let mut body = Vec::with_capacity(4 + args.len());
        body.extend_from_slice(&cmd.to_le_bytes());
        body.extend_from_slice(args);
        self.send_packet(PKT_COMMAND, &body, timeout)?;

        if cmd == CMD_COMPLETE {
            return Ok(None);
        }

        match self.recv_packet(timeout)? {
            Packet::Command {
                cmd: resp_cmd, payload, ..
            } => {
                if resp_cmd == CMD_COMPLETE {
                    let out_arg = if payload.len() >= 4 {
                        u32::from_le_bytes(payload[0..4].try_into().unwrap())
                    } else {
                        0
                    };
                    Ok(Some(out_arg))
                } else if resp_cmd == CMD_STATUS {
                    self.command_complete()?;
                    let code = if payload.len() >= 4 {
                        u32::from_le_bytes(payload[0..4].try_into().unwrap())
                    } else {
                        0
                    };
                    if code != 0 {
                        let msg = decode_status_msg(&payload);
                        return Err(format!("erreur cible NV3P (0x{code:08x}) : {msg}"));
                    }
                    Ok(None)
                } else {
                    Err(format!("réponse commande NV3P inattendue : {resp_cmd}"))
                }
            }
            Packet::Nack { code, .. } => Err(format!("commande NV3P refusée (NACK 0x{code:08x})")),
            other => Err(format!("paquet inattendu après commande NV3P : {other:?}")),
        }
    }

    fn get_command_status(&mut self, timeout: Duration) -> Result<(), String> {
        match self.recv_packet(timeout)? {
            Packet::Command { cmd, payload, .. } if cmd == CMD_STATUS => {
                self.command_complete()?;
                let code = if payload.len() >= 4 {
                    u32::from_le_bytes(payload[0..4].try_into().unwrap())
                } else {
                    0
                };
                if code != 0 {
                    let msg = decode_status_msg(&payload);
                    return Err(format!("statut cible NV3P (0x{code:08x}) : {msg}"));
                }
                Ok(())
            }
            other => Err(format!("statut NV3P attendu, reçu : {other:?}")),
        }
    }

    fn data_send(&mut self, data: &[u8], timeout: Duration) -> Result<(), String> {
        self.send_packet(PKT_DATA, data, timeout)
    }

    fn data_receive(&mut self, timeout: Duration) -> Result<Vec<u8>, String> {
        match self.recv_packet(timeout)? {
            Packet::Data { payload, .. } => Ok(payload),
            Packet::Command { cmd, payload, .. } if cmd == CMD_STATUS => {
                self.command_complete()?;
                let code = if payload.len() >= 4 {
                    u32::from_le_bytes(payload[0..4].try_into().unwrap())
                } else {
                    0
                };
                let msg = decode_status_msg(&payload);
                Err(format!("statut cible NV3P (0x{code:08x}) : {msg}"))
            }
            other => Err(format!("données NV3P attendues, reçu : {other:?}")),
        }
    }

    /// Queries the running recovery agent (`cmd 1`) with an explicit USB timeout.
    /// Returns `(mode, major, minor, build)`: `mode == 1` is `nvtboot_recovery`, `mode == 2` is CBoot.
    pub fn query_agent_timeout(&mut self, timeout: Duration) -> Result<(u32, u32, u32, u32), String> {
        let len = self
            .command_send_raw(CMD_IS_APPLET_OR_CBOOT, &[], timeout)?
            .unwrap_or(0);
        if len != 16 {
            return Err(format!("taille info agent inattendue : {len}"));
        }
        let data = self.data_receive(timeout)?;
        self.get_command_status(timeout)?;
        if data.len() < 16 {
            return Err("réponse info agent tronquée".into());
        }
        Ok((
            u32::from_le_bytes(data[0..4].try_into().unwrap()),
            u32::from_le_bytes(data[4..8].try_into().unwrap()),
            u32::from_le_bytes(data[8..12].try_into().unwrap()),
            u32::from_le_bytes(data[12..16].try_into().unwrap()),
        ))
    }

    /// Queries the running recovery agent (`cmd 1`).
    /// Returns `(mode, major, minor, build)`: `mode == 1` is `nvtboot_recovery`, `mode == 2` is CBoot.
    pub fn query_agent(&mut self) -> Result<(u32, u32, u32, u32), String> {
        self.query_agent_timeout(SHORT_TIMEOUT)
    }

    /// Checks whether the `nvtboot_recovery` applet is active (`mode == 1`).
    pub fn is_applet(&mut self) -> Result<Option<(u32, u32, u32)>, String> {
        let (mode, maj, min, bld) = self.query_agent()?;
        Ok((mode == 1).then_some((maj, min, bld)))
    }

    /// Checks whether CBoot in recovery mode is active (`mode == 2`) with an explicit USB timeout.
    pub fn is_cboot_timeout(&mut self, timeout: Duration) -> Result<Option<(u32, u32, u32)>, String> {
        let (mode, maj, min, bld) = self.query_agent_timeout(timeout)?;
        Ok((mode == 2).then_some((maj, min, bld)))
    }

    /// Checks whether CBoot in recovery mode is active (`mode == 2`).
    pub fn is_cboot(&mut self) -> Result<Option<(u32, u32, u32)>, String> {
        self.is_cboot_timeout(SHORT_TIMEOUT)
    }

    /// Downloads a binary to the `nvtboot_recovery` applet (`NvTegraRcmAppletWrite`, `cmd 3`).
    pub fn applet_write(&mut self, tag: &[u8; 3], load_addr: u32, entry_addr: u32, data: &[u8]) -> Result<(), String> {
        let mut args = [0u8; 16];
        args[0..3].copy_from_slice(tag);
        args[4..8].copy_from_slice(&load_addr.to_le_bytes());
        args[8..12].copy_from_slice(&entry_addr.to_le_bytes());
        args[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        self.command_send_raw(CMD_WRITE, &args, SHORT_TIMEOUT)?;
        for chunk in data.chunks(MAX_CHUNK) {
            self.data_send(chunk, IO_TIMEOUT)?;
        }
        self.get_command_status(SHORT_TIMEOUT)
    }

    /// Instructs the applet to boot (`NvTegraRcmAppletBoot`, `cmd 9`).
    pub fn applet_boot(&mut self, mode: BootMode) -> Result<(), String> {
        let args = (mode as u32).to_le_bytes();
        self.command_send_raw(CMD_BOOT, &args, SHORT_TIMEOUT)?;
        self.get_command_status(SHORT_TIMEOUT)
    }

    /// Reads an eMMC partition via CBoot (`NvTegraDevFlashReadPartition`, `cmd 2`).
    pub fn read_partition(&mut self, name: &str, mut on_progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
        let mut args = [0u8; 40];
        let nb = name.as_bytes();
        if nb.len() >= args.len() {
            return Err(format!("nom de partition trop long : {name}"));
        }
        args[..nb.len()].copy_from_slice(nb);

        let out_len = self.command_send_raw(CMD_READ, &args, SHORT_TIMEOUT)?.unwrap_or(0);
        if out_len != 8 {
            return Err(format!("taille de métadonnées partition inattendue : {out_len}"));
        }
        let size_buf = self.data_receive(SHORT_TIMEOUT)?;
        if size_buf.len() < 8 {
            return Err("taille de partition tronquée".into());
        }
        let total = u64::from_le_bytes(size_buf[0..8].try_into().unwrap());
        let mut out = Vec::with_capacity(total.min(64 * 1024 * 1024) as usize);
        let mut received = 0u64;
        while received < total {
            let chunk = self
                .data_receive(IO_TIMEOUT)
                .map_err(|e| format!("{e} (après {received}/{total} o)"))?;
            received += chunk.len() as u64;
            out.extend_from_slice(&chunk);
            on_progress(received, total);
        }
        self.get_command_status(IO_TIMEOUT)?;
        Ok(out)
    }

    /// Reads only the first `max_bytes` of a partition by streaming through CBoot
    /// (still consumes the full transfer if CBoot sends it all, returning the requested prefix
    /// and its total SHA-256).
    pub fn read_partition_head_and_hash(
        &mut self,
        name: &str,
        head_len: usize,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<(u64, Vec<u8>, [u8; 32]), String> {
        use sha2::{Digest, Sha256};
        let mut args = [0u8; 40];
        let nb = name.as_bytes();
        if nb.len() >= args.len() {
            return Err(format!("nom de partition trop long : {name}"));
        }
        args[..nb.len()].copy_from_slice(nb);

        let out_len = self.command_send_raw(CMD_READ, &args, SHORT_TIMEOUT)?.unwrap_or(0);
        if out_len != 8 {
            return Err(format!("taille de métadonnées partition inattendue : {out_len}"));
        }
        let size_buf = self.data_receive(SHORT_TIMEOUT)?;
        if size_buf.len() < 8 {
            return Err("taille de partition tronquée".into());
        }
        let total = u64::from_le_bytes(size_buf[0..8].try_into().unwrap());
        let mut head = Vec::with_capacity(head_len);
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        while received < total {
            let chunk = self
                .data_receive(IO_TIMEOUT)
                .map_err(|e| format!("{e} (après {received}/{total} o)"))?;
            if head.len() < head_len {
                let take = (head_len - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            hasher.update(&chunk);
            received += chunk.len() as u64;
            on_progress(received, total);
        }
        self.get_command_status(IO_TIMEOUT)?;
        Ok((total, head, hasher.finalize().into()))
    }

    /// Streams `total_size` bytes from `reader` into an eMMC partition via CBoot
    /// (`NvTegraDevFlashWritePartition`, `cmd 3`).
    pub fn write_partition_reader<R: Read>(
        &mut self,
        name: &str,
        total_size: u64,
        reader: &mut R,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<(), String> {
        let mut args = [0u8; 48];
        let nb = name.as_bytes();
        if nb.len() >= 40 {
            return Err(format!("nom de partition trop long : {name}"));
        }
        args[..nb.len()].copy_from_slice(nb);
        args[40..48].copy_from_slice(&total_size.to_le_bytes());

        self.command_send_raw(CMD_WRITE, &args, SHORT_TIMEOUT)?;
        let mut buf = vec![0u8; MAX_CHUNK];
        let mut sent = 0u64;
        while sent < total_size {
            let want = ((total_size - sent) as usize).min(MAX_CHUNK);
            let mut filled = 0;
            while filled < want {
                let n = reader
                    .read(&mut buf[filled..want])
                    .map_err(|e| format!("lecture flux image : {e}"))?;
                if n == 0 {
                    return Err(format!("fin de flux prématurée après {sent} octets sur {total_size}"));
                }
                filled += n;
            }
            self.data_send(&buf[..filled], IO_TIMEOUT)?;
            sent += filled as u64;
            on_progress(sent, total_size);
        }
        self.get_command_status(IO_TIMEOUT)
    }

    /// Erases a partition via CBoot (`NvTegraDevFlashErasePartition`, `cmd 4`).
    #[allow(dead_code)]
    pub fn erase_partition(&mut self, name: &str) -> Result<(), String> {
        let mut args = [0u8; 40];
        let nb = name.as_bytes();
        if nb.len() >= args.len() {
            return Err(format!("nom de partition trop long : {name}"));
        }
        args[..nb.len()].copy_from_slice(nb);
        self.command_send_raw(CMD_ERASE, &args, IO_TIMEOUT)?;
        self.get_command_status(IO_TIMEOUT)
    }

    /// Reboots the board (`NvTegraDevFlashReboot`, `cmd 6`: `0 = Coldboot`, `1 = Recovery`).
    pub fn reboot(&mut self, mode: RebootMode) -> Result<(), String> {
        let args = (mode as u32).to_le_bytes();
        self.command_send_raw(CMD_REBOOT, &args, SHORT_TIMEOUT)?;
        Ok(())
    }
}

fn decode_status_msg(payload: &[u8]) -> String {
    if payload.len() <= 4 {
        return String::new();
    }
    let slice = &payload[4..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_ignores_len_and_negates_header_plus_body() {
        let hdr = [2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]; // version=2, cmd=1, seq=0
        let body = [1u8, 0, 0, 0]; // cmd=1
        let csum = compute_checksum(&hdr, &body);
        assert_eq!(csum, 0u32.wrapping_sub(4));
        let frame = encode_packet(PKT_COMMAND, 0, &body);
        assert_eq!(frame.len(), 12 + 4 + 4 + 4);
        assert_eq!(&frame[0..12], &hdr);
        assert_eq!(u32::from_le_bytes(frame[12..16].try_into().unwrap()), 4);
        assert_eq!(&frame[16..20], &body);
        assert_eq!(u32::from_le_bytes(frame[20..24].try_into().unwrap()), csum);
    }

    #[test]
    fn ack_frame_has_no_length_word() {
        let frame = encode_packet(PKT_ACK, 3, &[]);
        assert_eq!(frame.len(), 16);
        assert_eq!(u32::from_le_bytes(frame[0..4].try_into().unwrap()), VERSION);
        assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), PKT_ACK);
        assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), 3);
        let csum = u32::from_le_bytes(frame[12..16].try_into().unwrap());
        let sum: u32 = frame[0..12].iter().map(|&b| b as u32).sum();
        assert_eq!(sum.wrapping_add(csum), 0);
    }
}

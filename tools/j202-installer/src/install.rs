//! End-to-end J202 installation in a background thread:
//!   0. File & configuration checks
//!   1. RCM handshake -> nvtboot_recovery -> BCT + CBoot + DTB -> CBoot recovery (NV3P)
//!   2. Backup of boot partitions (BCT, DTB, TOS, EKS, LNX, BMP, RP4) into the local cache
//!   3. Verification of existing L4T bootloader partitions
//!   4. Streaming decompression + in-place `/boot/j202-firstboot.conf` patching to `APP`
//!   5. Read-back verification (ext4 superblock & size)
//!   6. Cold reboot

use crate::download::Item;
use crate::l4t;
use crate::nv3p::{BootMode, Nv3p, RebootMode};
use crate::rcm::Rcm;
use crate::release::Release;
use crate::usb;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const FIRSTBOOT_PLACEHOLDER: &[u8] = b"# J202_FIRSTBOOT_PLACEHOLDER_V1\n";
pub const FIRSTBOOT_SLOT_LEN: usize = 4096;

pub const STEPS: [&str; 7] = [
    "Vérification des fichiers",
    "Démarrage de CBoot en recovery",
    "Sauvegarde de BCT, DTB et partitions boot",
    "Contrôle des chargeurs NVIDIA",
    "Écriture du système Omarchy",
    "Vérification par relecture",
    "Redémarrage",
];

pub struct Plan {
    pub release: Release,
    pub l4t: PathBuf,
    pub cache: PathBuf,
    pub files: Vec<Item>,
    /// `/boot/j202-firstboot.conf` written into the streamed ext4 image at the write step.
    pub firstboot_conf: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Waiting,
    Running,
    Done,
    Failed,
}

pub struct RunState {
    pub steps: Vec<State>,
    pub log: Vec<String>,
    pub progress: Option<(u64, u64)>,
    /// Some at the end
    pub finished: Option<Result<(), String>>,
    /// Something was written on the eMMC
    pub wrote: bool,
}

pub struct Run {
    pub state: Arc<Mutex<RunState>>,
    cancel: Arc<AtomicBool>,
}

impl Run {
    /// Takes effect between two steps: a step that writes is never cut in the middle.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Context {
    cboot: Option<Nv3p>,
    written_size: u64,
}

pub fn start(plan: Plan, ctx: eframe::egui::Context) -> Run {
    let state = Arc::new(Mutex::new(RunState {
        steps: vec![State::Waiting; STEPS.len()],
        log: Vec::new(),
        progress: None,
        finished: None,
        wrote: false,
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (s, c) = (state.clone(), cancel.clone());
    std::thread::spawn(move || {
        let log = |m: String| {
            s.lock().unwrap().log.push(m);
            ctx.request_repaint();
        };
        let set_progress = |p: Option<(u64, u64)>| {
            s.lock().unwrap().progress = p;
            ctx.request_repaint();
        };
        let mut run_ctx = Context {
            cboot: None,
            written_size: 0,
        };
        let mut result = Ok(());
        for (i, name) in STEPS.iter().enumerate() {
            if c.load(Ordering::Relaxed) {
                result = Err("Installation annulée.".into());
                break;
            }
            {
                let mut g = s.lock().unwrap();
                g.steps[i] = State::Running;
                g.progress = None;
            }
            log(format!("{name}…"));
            if i == 4 {
                s.lock().unwrap().wrote = true;
            }
            let r = step(i, &plan, &mut run_ctx, &log, &set_progress);
            {
                let mut g = s.lock().unwrap();
                g.steps[i] = if r.is_ok() { State::Done } else { State::Failed };
                g.progress = None;
            }
            if let Err(e) = r {
                log(e.clone());
                result = Err(e);
                break;
            }
        }
        let mut g = s.lock().unwrap();
        let wrote = g.wrote;
        g.finished = Some(result.map_err(|e| {
            if wrote {
                e
            } else {
                format!("{e}\nRien n'a été écrit sur la carte.")
            }
        }));
        drop(g);
        ctx.request_repaint();
    });
    Run { state, cancel }
}

fn step(
    i: usize,
    plan: &Plan,
    run_ctx: &mut Context,
    log: &dyn Fn(String),
    set_progress: &dyn Fn(Option<(u64, u64)>),
) -> Result<(), String> {
    match i {
        0 => check_files(plan, log),
        1 => boot_cboot_recovery(plan, run_ctx, log),
        2 => backup_boot(plan, run_ctx, log, set_progress),
        3 => verify_bootloaders(run_ctx, log),
        4 => write_rootfs(plan, run_ctx, log, set_progress),
        5 => verify_rootfs(run_ctx, log, set_progress),
        6 => reboot_board(run_ctx, log),
        _ => Err("Étape inconnue.".into()),
    }
}

fn check_files(plan: &Plan, log: &dyn Fn(String)) -> Result<(), String> {
    if !plan.l4t.is_file() {
        return Err("Les fichiers NVIDIA ne sont pas téléchargés.".into());
    }
    if let Some(f) = plan.files.iter().find(|f| !f.is_done()) {
        return Err(format!("{} n'est pas téléchargé.", f.dest.display()));
    }
    if image_paths(plan).is_none() {
        return Err(format!(
            "La release {} n'a pas d'image système (j202-rootfs-*.img.zst).",
            plan.release.tag
        ));
    }
    if plan.firstboot_conf.is_empty() {
        return Err("La configuration du compte est vide.".into());
    }
    if plan.firstboot_conf.len() > FIRSTBOOT_SLOT_LEN {
        return Err("La configuration du compte dépasse 4096 octets.".into());
    }
    log(format!("{} fichiers prêts, compte préparé.", plan.files.len() + 1));
    Ok(())
}

fn image_paths(plan: &Plan) -> Option<Vec<PathBuf>> {
    let assets = plan.release.image_parts();
    if assets.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(assets.len());
    for asset in assets {
        let p = plan
            .files
            .iter()
            .find(|f| f.dest.file_name().is_some_and(|n| n == asset.name.as_str()))
            .map(|f| f.dest.clone())?;
        out.push(p);
    }
    Some(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn boot_cboot_recovery(plan: &Plan, run_ctx: &mut Context, log: &dyn Fn(String)) -> Result<(), String> {
    log("Préparation des binaires NVIDIA (L4T R32.7.6)…".into());
    let payloads = l4t::load_recovery_payloads(&plan.l4t, &plan.cache)?;

    let apx = usb::find().ok_or("La carte n'est plus en recovery (APX absent).")?;
    if !apx.driver_ok() {
        return Err(format!(
            "Windows n'a pas de pilote WinUSB pour APX (pilote : {}).",
            apx.driver.as_deref().unwrap_or("aucun")
        ));
    }

    let device = usb::open()?;
    let rcm = Rcm::open(&device)?;
    if let Some(id) = rcm.try_read_device_id()? {
        log(format!("Carte identifiée (boot ROM), id {}.", hex(&id)));
        rcm.boot_applet(&payloads.nvtboot_recovery)?;
    } else {
        log("UID boot ROM déjà lu dans cette session, détection NV3P puis RCM…".into());
        rcm.clear_halt();
        let mut probe = Nv3p::new(rcm.into_interface());
        match probe.query_agent() {
            Ok((2, maj, min, bld)) => {
                log(format!("CBoot recovery déjà actif : v{maj:02}.{min:02}.{bld:04}."));
                run_ctx.cboot = Some(probe);
                return Ok(());
            }
            Ok((1, maj, min, bld)) => {
                log(format!("Applet déjà actif : v{maj:02}.{min:02}.{bld:04}."));
                let cboot = finish_applet_to_cboot(probe, &payloads, log)?;
                run_ctx.cboot = Some(cboot);
                return Ok(());
            }
            Ok((mode, _, _, _)) => {
                return Err(format!(
                    "Agent USB inattendu ({mode}). Débranche puis rebranche le 12 V en recovery."
                ));
            }
            Err(_) => {
                let rcm2 = Rcm::from_interface(probe.into_interface());
                rcm2.clear_halt();
                rcm2.boot_applet(&payloads.nvtboot_recovery)?;
                let nv3p = Nv3p::new(rcm2.into_interface());
                let cboot = finish_applet_to_cboot(nv3p, &payloads, log)?;
                run_ctx.cboot = Some(cboot);
                return Ok(());
            }
        }
    }

    let nv3p = Nv3p::new(rcm.into_interface());
    let cboot = finish_applet_to_cboot(nv3p, &payloads, log)?;
    run_ctx.cboot = Some(cboot);
    Ok(())
}

fn finish_applet_to_cboot(
    mut nv3p: Nv3p,
    payloads: &l4t::RecoveryPayloads,
    log: &dyn Fn(String),
) -> Result<Nv3p, String> {
    let (maj, min, bld) = nv3p
        .is_applet()?
        .ok_or("L'applet nvtboot_recovery n'a pas répondu via NV3P.")?;
    log(format!("Applet nvtboot_recovery actif (v{maj:02}.{min:02}.{bld:04})."));

    nv3p.applet_write(b"bct", 0, 0, &payloads.signed_bct)?;
    nv3p.applet_write(b"ebt", 0x92c0_0000, 0x92c0_0000, &payloads.signed_ebt)?;
    nv3p.applet_write(b"rp1", 0x8300_0000, 0x8300_0000, &payloads.signed_rp1)?;
    nv3p.applet_boot(BootMode::Recovery)?;
    drop(nv3p);

    std::thread::sleep(Duration::from_millis(1500));
    let mut last_err = None;
    for _ in 0..15 {
        match usb::open().and_then(|dev| Rcm::open(&dev).map(Rcm::into_interface)) {
            Ok(iface) => {
                let mut n = Nv3p::new(iface);
                match n.is_cboot_timeout(Duration::from_secs(2)) {
                    Ok(Some((cmaj, cmin, cbld))) => {
                        log(format!("CBoot recovery actif (v{cmaj:02}.{cmin:02}.{cbld:04})."));
                        return Ok(n);
                    }
                    Ok(None) => last_err = Some("agent différent de CBoot".into()),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(e) => last_err = Some(e),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "CBoot recovery n'a pas répondu après démarrage de l'applet ({}).",
        last_err.as_deref().unwrap_or("aucune réponse")
    ))
}

fn backup_boot(
    plan: &Plan,
    run_ctx: &mut Context,
    log: &dyn Fn(String),
    set_progress: &dyn Fn(Option<(u64, u64)>),
) -> Result<(), String> {
    let cboot = run_ctx.cboot.as_mut().ok_or("CBoot recovery non initialisé.")?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = plan.cache.join("backups").join(format!("j202-{stamp}"));
    fs::create_dir_all(&dir).map_err(|e| format!("{} : {e}", dir.display()))?;

    let mut backed_bct = false;
    let mut backed_dtb = false;
    for part in ["BCT", "DTB", "TOS", "EKS", "LNX", "BMP", "RP4"] {
        let data = match cboot.read_partition(part, |done, total| {
            set_progress(Some((done, total)));
        }) {
            Ok(d) => d,
            Err(e) if e.contains("Partition not found") => {
                log(format!("Partition {part} absente (ignorée)."));
                continue;
            }
            Err(e) => return Err(e),
        };
        let dest = dir.join(format!("{part}.bin"));
        fs::write(&dest, &data).map_err(|e| format!("{} : {e}", dest.display()))?;
        let sha = Sha256::digest(&data);
        log(format!(
            "Sauvegardé {part} ({} o, SHA-256 {})",
            data.len(),
            &hex(&sha)[..16]
        ));
        if part == "BCT" && !data.is_empty() {
            backed_bct = true;
        }
        if part == "DTB" && !data.is_empty() {
            backed_dtb = true;
        }
    }
    if !backed_bct || !backed_dtb {
        return Err("Partitions obligatoires BCT ou DTB introuvables pour la sauvegarde.".into());
    }
    log(format!("Sauvegardes écrites dans {}.", dir.display()));
    Ok(())
}

fn verify_bootloaders(run_ctx: &mut Context, log: &dyn Fn(String)) -> Result<(), String> {
    let cboot = run_ctx.cboot.as_mut().ok_or("CBoot recovery non initialisé.")?;
    let bct = cboot.read_partition("BCT", |_, _| {})?;
    if bct.is_empty() {
        return Err("Partition BCT vide.".into());
    }
    let dtb = cboot.read_partition("DTB", |_, _| {})?;
    if dtb.is_empty() {
        return Err("Partition DTB vide.".into());
    }
    log(format!(
        "Chargeurs/boot eMMC vérifiés (BCT {} o, DTB {} o).",
        bct.len(),
        dtb.len()
    ));
    Ok(())
}

fn write_rootfs(
    plan: &Plan,
    run_ctx: &mut Context,
    log: &dyn Fn(String),
    set_progress: &dyn Fn(Option<(u64, u64)>),
) -> Result<(), String> {
    let parts = image_paths(plan).ok_or("Image système introuvable.")?;
    log("Calcul de la taille décompressée de l'image…".into());
    let total = zstd_uncompressed_size(&parts)?;
    run_ctx.written_size = total;
    let first_name = parts[0].file_name().unwrap_or_default().to_string_lossy();
    if parts.len() == 1 {
        log(format!(
            "Écriture de {first_name} ({} Mio) sur APP…",
            total / (1024 * 1024)
        ));
    } else {
        log(format!(
            "Écriture de {first_name} ({} parties, {} Mio) sur APP…",
            parts.len(),
            total / (1024 * 1024)
        ));
    }

    let reader = MultiFileReader::new(parts);
    let dec = zstd::Decoder::new(BufReader::new(reader)).map_err(|e| format!("décodeur zstd : {e}"))?;
    let mut patched = FirstbootPatchReader::new(dec, &plan.firstboot_conf)?;

    let cboot = run_ctx.cboot.as_mut().ok_or("CBoot recovery non initialisé.")?;
    cboot.write_partition_reader("APP", total, &mut patched, |done, tot| {
        set_progress(Some((done, tot)));
    })?;

    if !patched.patched() {
        return Err("Emplacement /boot/j202-firstboot.conf introuvable dans l'image : image incompatible.".into());
    }
    log("Image écrite et configuration de premier démarrage injectée.".into());
    Ok(())
}

fn verify_rootfs(
    run_ctx: &mut Context,
    log: &dyn Fn(String),
    set_progress: &dyn Fn(Option<(u64, u64)>),
) -> Result<(), String> {
    let cboot = run_ctx.cboot.as_mut().ok_or("CBoot recovery non initialisé.")?;
    let (total, head, sha) = cboot.read_partition_head_and_hash("APP", 4096, |done, tot| {
        set_progress(Some((done, tot)));
    })?;
    if total < run_ctx.written_size {
        return Err(format!(
            "Partition APP trop petite ({total} o < {} o).",
            run_ctx.written_size
        ));
    }
    // ext4 superblock sits at offset 1024; magic 0xEF53 LE sits at 1024 + 0x38 = 0x438.
    if head.len() < 0x43a || head[0x438] != 0x53 || head[0x439] != 0xef {
        return Err("Signature ext4 (0xEF53) absente après écriture de APP.".into());
    }
    log(format!(
        "Partition APP vérifiée ({} Mio, ext4 OK, SHA-256 {}).",
        total / (1024 * 1024),
        &hex(&sha)[..16]
    ));
    Ok(())
}

fn reboot_board(run_ctx: &mut Context, log: &dyn Fn(String)) -> Result<(), String> {
    let cboot = run_ctx.cboot.as_mut().ok_or("CBoot recovery non initialisé.")?;
    cboot.reboot(RebootMode::Coldboot)?;
    log("Commande de redémarrage envoyée.".into());
    Ok(())
}

/// Formats `firstboot_conf` into an exact 4096-byte slot padded with `#` comments.
pub fn format_firstboot_slot(conf: &str) -> Result<[u8; FIRSTBOOT_SLOT_LEN], String> {
    let bytes = conf.as_bytes();
    if bytes.len() > FIRSTBOOT_SLOT_LEN {
        return Err("configuration firstboot > 4096 octets".into());
    }
    let mut slot = [b'#'; FIRSTBOOT_SLOT_LEN];
    slot[..bytes.len()].copy_from_slice(bytes);
    if !bytes.ends_with(b"\n") && bytes.len() < FIRSTBOOT_SLOT_LEN {
        slot[bytes.len()] = b'\n';
    }
    slot[FIRSTBOOT_SLOT_LEN - 1] = b'\n';
    Ok(slot)
}

/// Streaming reader wrapper that locates the 4096-byte `# J202_FIRSTBOOT_PLACEHOLDER_V1\n`
/// placeholder block in an ext4 image stream and replaces it in place with `slot`.
pub struct FirstbootPatchReader<R: Read> {
    inner: R,
    slot: [u8; FIRSTBOOT_SLOT_LEN],
    buf: Vec<u8>,
    eof: bool,
    patched: bool,
}

impl<R: Read> FirstbootPatchReader<R> {
    pub fn new(inner: R, firstboot_conf: &str) -> Result<Self, String> {
        Ok(Self {
            inner,
            slot: format_firstboot_slot(firstboot_conf)?,
            buf: Vec::with_capacity(64 * 1024),
            eof: false,
            patched: false,
        })
    }

    pub fn patched(&self) -> bool {
        self.patched
    }

    fn fill_until(&mut self, target: usize) -> io::Result<()> {
        let mut tmp = [0u8; 32 * 1024];
        while !self.eof && self.buf.len() < target {
            let n = self.inner.read(&mut tmp)?;
            if n == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&tmp[..n]);
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for FirstbootPatchReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        // Keep enough lookahead to never split a 4096-byte placeholder across the safe emit boundary.
        let min_buf = FIRSTBOOT_SLOT_LEN + out.len();
        self.fill_until(min_buf)?;

        if !self.patched
            && self.buf.len() >= FIRSTBOOT_PLACEHOLDER.len()
            && let Some(pos) = self
                .buf
                .windows(FIRSTBOOT_PLACEHOLDER.len())
                .position(|w| w == FIRSTBOOT_PLACEHOLDER)
        {
            // Ensure the full 4096-byte slot is buffered before patching.
            self.fill_until(pos + FIRSTBOOT_SLOT_LEN)?;
            if pos + FIRSTBOOT_SLOT_LEN <= self.buf.len() {
                let tail_ok = self.buf[pos + FIRSTBOOT_PLACEHOLDER.len()..pos + FIRSTBOOT_SLOT_LEN]
                    .iter()
                    .all(|&b| b == b'#' || b == b'\n');
                if tail_ok {
                    self.buf[pos..pos + FIRSTBOOT_SLOT_LEN].copy_from_slice(&self.slot);
                    self.patched = true;
                }
            }
        }

        let safe_avail = if self.eof || self.patched {
            self.buf.len()
        } else {
            self.buf.len().saturating_sub(FIRSTBOOT_SLOT_LEN)
        };
        let take = safe_avail.min(out.len());
        if take == 0 && !self.eof {
            // Read more until either patched, safe bytes appear, or EOF.
            self.fill_until(self.buf.len() + FIRSTBOOT_SLOT_LEN)?;
            return self.read(out);
        }
        out[..take].copy_from_slice(&self.buf[..take]);
        self.buf.drain(..take);
        Ok(take)
    }
}

/// Streams a sequence of files as one contiguous `Read` stream (supports split `.img.zst.partNN`).
pub struct MultiFileReader {
    files: Vec<PathBuf>,
    idx: usize,
    cur: Option<File>,
}

impl MultiFileReader {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            idx: 0,
            cur: None,
        }
    }
}

impl Read for MultiFileReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.cur.is_none() {
                if self.idx >= self.files.len() {
                    return Ok(0);
                }
                let p = &self.files[self.idx];
                self.cur = Some(File::open(p)?);
                self.idx += 1;
            }
            let n = self.cur.as_mut().unwrap().read(out)?;
            if n > 0 {
                return Ok(n);
            }
            self.cur = None;
        }
    }
}

/// Determines the uncompressed byte size of a single or multi-part `.zst` image.
pub fn zstd_uncompressed_size(paths: &[PathBuf]) -> Result<u64, String> {
    if paths.is_empty() {
        return Err("Image système introuvable.".into());
    }
    let reader = MultiFileReader::new(paths.to_vec());
    let mut dec = zstd::Decoder::new(BufReader::new(reader)).map_err(|e| format!("décodeur zstd : {e}"))?;
    io::copy(&mut dec, &mut io::sink()).map_err(|e| format!("lecture de {} : {e}", paths[0].display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct ChunkedReader<'a> {
        data: &'a [u8],
        pos: usize,
        chunk: usize,
    }

    impl<'a> Read for ChunkedReader<'a> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = (self.data.len() - self.pos).min(out.len()).min(self.chunk);
            out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn firstboot_patch_reader_replaces_split_placeholder_preserving_length() {
        let mut img = vec![0xAAu8; 10_000];
        let offset = 4096;
        img[offset..offset + FIRSTBOOT_PLACEHOLDER.len()].copy_from_slice(FIRSTBOOT_PLACEHOLDER);
        img[offset + FIRSTBOOT_PLACEHOLDER.len()..offset + FIRSTBOOT_SLOT_LEN].fill(b'#');
        img[offset + FIRSTBOOT_SLOT_LEN - 1] = b'\n';

        let conf = "J202_USER=kevin\nJ202_HOST=nano\nJ202_PASS_HASH='$6$abc$xyz'\n";
        // Tiny chunk size (77 bytes) forces split across header and read boundaries.
        let src = ChunkedReader {
            data: &img,
            pos: 0,
            chunk: 77,
        };
        let mut reader = FirstbootPatchReader::new(src, conf).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 113];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert!(reader.patched());
        assert_eq!(out.len(), img.len());
        assert!(out[..offset].iter().all(|&b| b == 0xAA));
        assert!(out[offset + FIRSTBOOT_SLOT_LEN..].iter().all(|&b| b == 0xAA));
        let slot_str = std::str::from_utf8(&out[offset..offset + FIRSTBOOT_SLOT_LEN]).unwrap();
        assert!(slot_str.starts_with(conf));
        assert!(!slot_str.contains("J202_FIRSTBOOT_PLACEHOLDER_V1"));
    }

    #[test]
    fn zstd_size_matches_roundtrip() {
        let dir = std::env::temp_dir().join(format!("j202-zst-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.img.zst");
        let payload = vec![0x42u8; 123_456];
        let compressed = zstd::encode_all(Cursor::new(&payload), 3).unwrap();
        fs::write(&path, &compressed).unwrap();
        assert_eq!(zstd_uncompressed_size(std::slice::from_ref(&path)).unwrap(), 123_456);

        let mid = compressed.len() / 2;
        let p1 = dir.join("t.img.zst.part01");
        let p2 = dir.join("t.img.zst.part02");
        fs::write(&p1, &compressed[..mid]).unwrap();
        fs::write(&p2, &compressed[mid..]).unwrap();
        assert_eq!(zstd_uncompressed_size(&[p1, p2]).unwrap(), 123_456);
        fs::remove_dir_all(&dir).unwrap();
    }
}

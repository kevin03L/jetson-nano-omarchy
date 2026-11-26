//! Read-only end-to-end check on a real board in recovery:
//!   1. Opens APX (0955:7f21), reads the 16-byte device id.
//!   2. Runs the official T210 RCM handshake (`QueryVersion` + `nvtboot_recovery.bin`).
//!   3. Talks NV3P to the applet (`is_applet`), sends the signed BCT, CBoot (`EBT`), and DTB (`RP1`),
//!      then boots CBoot in recovery (`BootMode::Recovery`).
//!   4. Waits for CBoot (`is_cboot`), then reads `BCT` and `PT` partitions (STRICTLY READ-ONLY).
//!
//! Optional flag: `--reboot-recovery` at the end to return to APX BootROM without unplugging 12 V.
//!
//! Run with:
//!   cargo run --example probe_cboot [path/to/l4t.tbz2] [--reboot-recovery]

use j202_installer::l4t;
use j202_installer::nv3p::{BootMode, Nv3p, RebootMode};
use j202_installer::rcm::Rcm;
use j202_installer::usb;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reboot_recovery = args.iter().any(|a| a == "--reboot-recovery");
    let l4t_path = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Lerou\Documents\jetson\l4t-bsp\l4t.tbz2"));

    println!("Chargement des binaires L4T depuis {}…", l4t_path.display());
    let payloads = l4t::load_recovery_payloads(&l4t_path, &l4t::default_cache_dir())
        .unwrap_or_else(|e| panic!("échec chargement L4T : {e}"));
    println!(
        "Payloads prêts : nvtboot_recovery={} o, bct={} o, ebt={} o, rp1={} o",
        payloads.nvtboot_recovery.len(),
        payloads.signed_bct.len(),
        payloads.signed_ebt.len(),
        payloads.signed_rp1.len()
    );

    let dev = usb::open().unwrap_or_else(|e| panic!("APX : {e}"));
    let rcm = Rcm::open(&dev).unwrap_or_else(|e| panic!("RCM open : {e}"));

    enum Agent {
        Applet(Nv3p),
        CBoot(Nv3p),
    }

    let uid = rcm.try_read_device_id().unwrap_or_else(|e| panic!("UID : {e}"));
    let agent = match uid {
        Some(id) => {
            println!("BootROM UID : {}", hex(&id));
            println!("Envoi QueryVersion + nvtboot_recovery.bin via RCM…");
            rcm.boot_applet(&payloads.nvtboot_recovery)
                .unwrap_or_else(|e| panic!("boot_applet : {e}"));
            Agent::Applet(Nv3p::new(rcm.into_interface()))
        }
        None => {
            println!("BootROM UID déjà consommé dans cette session (test NV3P puis QueryVersion RCM)…");
            rcm.clear_halt();
            let mut n = Nv3p::new(rcm.into_interface());
            match n.query_agent() {
                Ok((2, maj, min, bld)) => {
                    println!("CBoot recovery déjà actif : version {maj:02}.{min:02}.{bld:04}");
                    Agent::CBoot(n)
                }
                Ok((1, maj, min, bld)) => {
                    println!("Applet déjà actif : version {maj:02}.{min:02}.{bld:04}");
                    Agent::Applet(n)
                }
                nv3p_err => {
                    println!("NV3P inactif ({nv3p_err:?}), tentative QueryVersion + boot_applet RCM…");
                    let rcm2 = Rcm::from_interface(n.into_interface());
                    rcm2.clear_halt();
                    rcm2.boot_applet(&payloads.nvtboot_recovery).unwrap_or_else(|e| {
                        panic!("ni NV3P ni BootROM RCM ne répondent ({e}) — si le boot ROM a bloqué sur un essai précédent, débranche/rebranche le 12 V")
                    });
                    Agent::Applet(Nv3p::new(rcm2.into_interface()))
                }
            }
        }
    };

    let mut cboot = match agent {
        Agent::CBoot(c) => c,
        Agent::Applet(mut nv3p) => {
            let (maj, min, bld) = nv3p
                .is_applet()
                .unwrap_or_else(|e| panic!("is_applet : {e}"))
                .expect("l'applet nvtboot_recovery ne répond pas");
            println!("Applet actif : version {maj:02}.{min:02}.{bld:04}");

            println!("Envoi BCT, CBoot (EBT) et DTB (RP1) via NV3P…");
            nv3p.applet_write(b"bct", 0, 0, &payloads.signed_bct)
                .unwrap_or_else(|e| panic!("applet_write bct : {e}"));
            nv3p.applet_write(b"ebt", 0x92c0_0000, 0x92c0_0000, &payloads.signed_ebt)
                .unwrap_or_else(|e| panic!("applet_write ebt : {e}"));
            nv3p.applet_write(b"rp1", 0x8300_0000, 0x8300_0000, &payloads.signed_rp1)
                .unwrap_or_else(|e| panic!("applet_write rp1 : {e}"));

            println!("Démarrage de CBoot en mode recovery…");
            nv3p.applet_boot(BootMode::Recovery)
                .unwrap_or_else(|e| panic!("applet_boot : {e}"));
            drop(nv3p);

            // CBoot re-enumerates or takes a moment to initialize USB; poll with short timeouts.
            std::thread::sleep(Duration::from_millis(1500));
            let mut cboot_nv3p = None;
            let mut last_err = None;
            for attempt in 0..15 {
                match usb::open().and_then(|dev2| Rcm::open(&dev2).map(Rcm::into_interface)) {
                    Ok(iface) => {
                        let mut n = Nv3p::new(iface);
                        match n.is_cboot_timeout(Duration::from_secs(2)) {
                            Ok(Some((cmaj, cmin, cbld))) => {
                                println!(
                                    "CBoot recovery actif (essai {attempt}) : version {cmaj:02}.{cmin:02}.{cbld:04}"
                                );
                                cboot_nv3p = Some(n);
                                break;
                            }
                            Ok(None) => {
                                eprintln!("  essai {attempt} : agent différent de CBoot");
                                last_err = Some("agent différent de CBoot".into());
                            }
                            Err(e) => {
                                eprintln!("  essai {attempt} : {e}");
                                last_err = Some(e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  essai {attempt} (open) : {e}");
                        last_err = Some(e);
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if let Some(c) = cboot_nv3p {
                c
            } else {
                panic!(
                    "CBoot recovery n'a pas répondu dans les ~35 s ({})",
                    last_err.as_deref().unwrap_or("aucune réponse")
                );
            }
        }
    };

    let mut saw_bct = false;
    let mut saw_dtb = false;
    for part in ["BCT", "DTB", "TOS", "EKS", "LNX", "BMP", "RP4"] {
        match cboot.read_partition(part, |_, _| {}) {
            Ok(data) => {
                let sha = Sha256::digest(&data);
                println!("Partition {part} lue : {} octets, SHA-256 = {}", data.len(), hex(&sha));
                if part == "BCT" && !data.is_empty() {
                    saw_bct = true;
                }
                if part == "DTB" && !data.is_empty() {
                    saw_dtb = true;
                }
            }
            Err(e) => {
                println!("Partition {part} : {e}");
            }
        }
    }

    if reboot_recovery {
        println!("Redémarrage en recovery (APX)…");
        cboot
            .reboot(RebootMode::Recovery)
            .unwrap_or_else(|e| panic!("reboot recovery : {e}"));
    }

    assert!(saw_bct, "partition BCT manquante ou vide sur eMMC");
    assert!(saw_dtb, "partition DTB manquante ou vide sur eMMC");
    println!("SUCCÈS : chaîne RCM -> Applet -> CBoot -> lecture eMMC vérifiée (rien écrit).");
}

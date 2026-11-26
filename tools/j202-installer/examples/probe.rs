//! Read-only check against a board in recovery: open APX and read its 16-byte device id through the
//! boot ROM. Mirrors src/rcm.rs; writes nothing. Run with the board in recovery and WinUSB bound:
//!   cargo run --example probe
use nusb::MaybeFuture;
use nusb::transfer::{Bulk, In};

const VID: u16 = 0x0955;
const PID: u16 = 0x7f21;

fn main() {
    let info = match nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
    {
        Some(d) => d,
        None => {
            eprintln!("APX (0955:7f21) absent : la carte n'est pas en recovery, ou WinUSB n'est pas lié.");
            std::process::exit(1);
        }
    };
    println!("APX trouvé, pilote Windows : {:?}", info.driver());
    let device = info.open().wait().expect("ouverture");
    let interface = device.claim_interface(0).wait().expect("claim interface 0");
    let ep = interface.endpoint::<Bulk, In>(0x81).expect("endpoint 0x81 in");
    let mut reader = ep.reader(64);
    let mut id = [0u8; 16];
    std::io::Read::read_exact(&mut reader, &mut id).expect("lecture de l'identifiant");
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    println!("Identifiant de la carte (16 o) : {hex}");
    println!("OK : dialogue RCM en lecture seule réussi, rien n'a été écrit.");
}

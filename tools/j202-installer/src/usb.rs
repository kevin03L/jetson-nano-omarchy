//! The board in recovery: the Tegra210 boot ROM, "APX", USB 0955:7f21.

use nusb::MaybeFuture;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const VID: u16 = 0x0955;
pub const PID: u16 = 0x7f21;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Apx {
    /// Windows' driver for the device, when it has one.
    pub driver: Option<String>,
}

impl Apx {
    /// A driver through which a program can talk to the device.
    pub fn driver_ok(&self) -> bool {
        self.driver
            .as_deref()
            .is_some_and(|d| ["winusb", "libusbk", "libusb0"].contains(&d.to_ascii_lowercase().as_str()))
    }
}

pub fn find() -> Option<Apx> {
    nusb::list_devices()
        .wait()
        .ok()?
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
        .map(|d| Apx { driver: driver(&d) })
}

/// Opens the board in recovery, to talk to its boot ROM.
pub fn open() -> Result<nusb::Device, String> {
    let info = nusb::list_devices()
        .wait()
        .map_err(|e| format!("liste des périphériques USB : {e}"))?
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
        .ok_or("La carte n'est plus en recovery (APX absent).")?;
    info.open()
        .wait()
        .map_err(|e| format!("ouverture de la carte en recovery : {e}"))
}

#[cfg(windows)]
fn driver(d: &nusb::DeviceInfo) -> Option<String> {
    d.driver().map(str::to_owned)
}

#[cfg(not(windows))]
fn driver(_: &nusb::DeviceInfo) -> Option<String> {
    None
}

/// Is the board physically on the USB bus, whatever driver it has — or none at all? nusb cannot see
/// APX until WinUSB is bound to it, so the "needs a driver" state needs this lower-level check. It
/// reads the hardware id list from Windows' configuration manager; nothing is opened or changed.
#[cfg(windows)]
pub fn present() -> bool {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CR_SUCCESS,
    };
    use windows::core::w;
    let filter = w!("USB");
    unsafe {
        let mut len: u32 = 0;
        if CM_Get_Device_ID_List_SizeW(&mut len, filter, CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS || len == 0 {
            return false;
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(filter, &mut buf, CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS {
            return false;
        }
        String::from_utf16_lossy(&buf)
            .to_ascii_uppercase()
            .contains(&format!("VID_{VID:04X}&PID_{PID:04X}"))
    }
}

#[cfg(not(windows))]
pub fn present() -> bool {
    false
}

/// What the recovery step sees. nusb binds only once a usable driver is present, so a board with no
/// driver shows as `NeedsDriver`, detected through `present()`.
#[derive(Clone, PartialEq, Eq)]
pub enum Recovery {
    Absent,
    NeedsDriver(Option<String>),
    Ready(Apx),
}

fn look() -> Recovery {
    match find() {
        Some(a) if a.driver_ok() => Recovery::Ready(a),
        Some(a) => Recovery::NeedsDriver(a.driver),
        None if present() => Recovery::NeedsDriver(None),
        None => Recovery::Absent,
    }
}

/// Looks for the board every second; the window is repainted when its state changes.
pub fn watch(ctx: eframe::egui::Context) -> Arc<Mutex<Recovery>> {
    let seen = Arc::new(Mutex::new(Recovery::Absent));
    let shared = seen.clone();
    std::thread::spawn(move || {
        loop {
            let now = look();
            let mut s = shared.lock().unwrap();
            if *s != now {
                *s = now;
                ctx.request_repaint();
            }
            drop(s);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drivers() {
        let a = |d: Option<&str>| Apx {
            driver: d.map(str::to_owned),
        };
        assert!(a(Some("WinUSB")).driver_ok());
        assert!(a(Some("libusbK")).driver_ok());
        assert!(!a(Some("usbccgp")).driver_ok());
        assert!(!a(None).driver_ok());
    }
}

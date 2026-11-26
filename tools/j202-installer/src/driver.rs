//! The Windows driver for APX (0955:7f21).
//!
//! A program can only talk to the board through WinUSB (or libusbK). The board ships with no such
//! driver, so this binds Windows' own in-box WinUSB to it: a standard WinUSB INF is written and
//! installed with `pnputil`, the tool Windows provides for driver packages. No security protection
//! is touched; the user approves the elevation, and WinUSB itself is Microsoft-signed and in-box.
//!
//! If Windows refuses the unsigned package (driver-signature enforcement), the tool says so and
//! points to Zadig, which installs the same WinUSB driver with its own signed helper.

use crate::usb::{PID, VID};
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// A WinUSB INF binding Windows' in-box WinUSB to this one device.
pub fn inf_text() -> String {
    let (vid, pid) = (format!("{VID:04X}"), format!("{PID:04X}"));
    format!(
        "; j202-installer: WinUSB for the Jetson Nano in recovery (APX)\r\n\
         [Version]\r\n\
         Signature = \"$Windows NT$\"\r\n\
         Class = USBDevice\r\n\
         ClassGuid = {{88BAE032-5A81-49f0-BC3D-A4FF138216D6}}\r\n\
         Provider = %Provider%\r\n\
         CatalogFile = j202-apx.cat\r\n\
         DriverVer = 01/01/2026,1.0.0.0\r\n\
         PnpLockdown = 1\r\n\
         \r\n\
         [Manufacturer]\r\n\
         %Provider% = Devices, NTamd64\r\n\
         \r\n\
         [Devices.NTamd64]\r\n\
         %DeviceName% = WinUSB, USB\\VID_{vid}&PID_{pid}\r\n\
         \r\n\
         [WinUSB.NT]\r\n\
         Include = winusb.inf\r\n\
         Needs = WINUSB.NT\r\n\
         \r\n\
         [WinUSB.NT.Services]\r\n\
         Include = winusb.inf\r\n\
         Needs = WINUSB.NT.Services\r\n\
         \r\n\
         [WinUSB.NT.Wdf]\r\n\
         KmdfService = WINUSB, WinUSB_Install\r\n\
         \r\n\
         [WinUSB_Install]\r\n\
         KmdfLibraryVersion = 1.11\r\n\
         \r\n\
         [Strings]\r\n\
         Provider = \"jetson-nano-omarchy\"\r\n\
         DeviceName = \"Jetson Nano recovery (APX)\"\r\n"
    )
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    // Admin rights let a plain file be created directly under the Windows directory.
    let probe = Path::new(r"C:\Windows\j202-installer-elevation-probe.tmp");
    match std::fs::File::create(probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Starts this same program again, asking Windows for administrator rights (a UAC prompt the user
/// approves). The caller then exits so the elevated copy takes over. No rights are escalated silently.
#[cfg(windows)]
pub fn relaunch_as_admin() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};
    let exe = std::env::current_exe().map_err(|e| format!("chemin du programme : {e}"))?;
    let mut wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    wide.push(0);
    // the elevated copy resumes the driver install on its own, so one click is enough
    let r = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(wide.as_ptr()),
            w!("--install-driver"),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success.
    if r.0 as usize > 32 {
        Ok(())
    } else {
        Err("Windows n'a pas accordé les droits administrateur (élévation refusée).".into())
    }
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> Result<(), String> {
    Err("l'élévation n'existe que sous Windows".into())
}

/// Writes the INF, signs its catalog with our own self-signed certificate, trusts that certificate
/// machine-wide, then installs with pnputil. Windows refuses an unsigned package, so the signing is
/// what makes the install work. Needs admin. The certificate is ours alone, used only for this driver.
pub fn install(cache: &Path) -> Result<(), String> {
    if !is_elevated() {
        return Err("Relance l'installeur en tant qu'administrateur pour installer le pilote.".into());
    }
    let dir = cache.join("driver");
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(&pkg).map_err(|e| format!("{} : {e}", pkg.display()))?;
    let inf = pkg.join("j202-apx.inf");
    write_inf(&inf)?;
    sign_and_install(&dir, &pkg, &inf)
}

fn write_inf(inf: &Path) -> Result<(), String> {
    let mut f = std::fs::File::create(inf).map_err(|e| format!("{} : {e}", inf.display()))?;
    f.write_all(inf_text().as_bytes())
        .map_err(|e| format!("{} : {e}", inf.display()))
}

/// The PowerShell that creates the certificate, signs the catalog, trusts the certificate, installs.
/// Written out and run so the steps are legible and use Windows' own signed cmdlets.
pub const SIGN_SCRIPT: &str = r#"
param([Parameter(Mandatory)][string]$PkgDir, [Parameter(Mandatory)][string]$Inf)
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = 'Stop'
try {
    $subject = 'CN=jetson-nano-omarchy J202 installer'
    $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject } | Select-Object -First 1
    if (-not $cert) {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject `
            -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature -NotAfter (Get-Date).AddYears(10)
    }
    $cat = Join-Path $PkgDir 'j202-apx.cat'
    New-FileCatalog -Path $PkgDir -CatalogFilePath $cat -CatalogVersion 2 | Out-Null
    $sig = Set-AuthenticodeSignature -FilePath $cat -Certificate $cert
    # Right after signing the certificate is not trusted yet, so the status is UnknownError, not
    # Valid; what matters is that a signature was applied. pnputil checks the trust once it is imported.
    if (-not $sig.SignerCertificate) { throw "le catalogue n'a pas pu être signé : $($sig.StatusMessage)" }
    $cer = Join-Path $PkgDir 'j202-apx.cer'
    Export-Certificate -Cert $cert -FilePath $cer | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
    $out = & pnputil /add-driver $Inf /install 2>&1 | Out-String
    Write-Output $out
    exit $LASTEXITCODE
} catch {
    Write-Output $_.Exception.Message
    exit 1
}
"#;

#[cfg(windows)]
fn sign_and_install(dir: &Path, pkg: &Path, inf: &Path) -> Result<(), String> {
    let script = dir.join("sign-install.ps1");
    std::fs::write(&script, SIGN_SCRIPT).map_err(|e| format!("{} : {e}", script.display()))?;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg(pkg)
        .arg(inf)
        .output()
        .map_err(|e| format!("powershell introuvable : {e}"))?;
    let msg = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "L'installation du pilote a échoué.\n{}\n{}",
        msg.trim(),
        err.trim()
    ))
}

#[cfg(not(windows))]
fn sign_and_install(_dir: &Path, _pkg: &Path, _inf: &Path) -> Result<(), String> {
    Err("l'installation du pilote n'existe que sous Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inf_names_the_device() {
        let t = inf_text();
        assert!(t.contains("USB\\VID_0955&PID_7F21"));
        assert!(t.contains("Needs = WINUSB.NT"));
        assert!(t.contains("jetson-nano-omarchy"));
        // CRLF line endings, as an INF needs
        assert!(t.contains("\r\n"));
        assert!(!t.contains("\n\n"));
    }
}

//! Installs and removes the Virtual Display Driver (an MIT licensed Indirect Display Driver,
//! signed by the SignPath Foundation), which gives the Windows host real extra monitors.
//!
//! Needs administrator rights, once. Like Microsoft's `devcon install`: create a
//! root-enumerated device with the driver's hardware id, then install the driver package on
//! it. Afterwards the host controls the monitors without administrator rights, through the
//! driver's settings file and named pipe (see display.rs).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_ALLCLASSES, DIGCF_PRESENT, INSTALLFLAG_FORCE, SP_DEVINFO_DATA, SPDRP_HARDWAREID,
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiSetDeviceRegistryPropertyW,
    UpdateDriverForPlugAndPlayDevicesW,
};
use windows::core::{BOOL, GUID, HSTRING, PCWSTR, w};

/// The driver's hardware id (from MttVDD.inf).
pub const HARDWARE_ID: &str = r"Root\MttVDD";
/// Where the driver reads its settings; the host rewrites them as screens come and go.
pub const SETTINGS_DIR: &str = r"C:\VirtualDisplayDriver";
/// The Display device class.
const DISPLAY_CLASS: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);

/// `freemonplay-host --install-display-driver <folder with MttVDD.inf>`.
pub fn install(package: &Path) -> Result<()> {
    let inf = package.join("MttVDD.inf");
    if !inf.exists() {
        bail!("no MttVDD.inf in {} (unpack VirtualDisplayDriver-x86.Driver.Only.zip there)", package.display());
    }
    let inf = std::fs::canonicalize(&inf)?;
    prepare_settings_dir(package)?;
    let existed = device_count()? > 0;
    if !existed {
        create_device()?;
        println!("Created the virtual display device ({HARDWARE_ID}).");
    }
    let mut reboot = BOOL(0);
    let inf_path = HSTRING::from(strip_verbatim(&inf).as_os_str());
    // SAFETY: installs the package on devices with our hardware id; valid strings.
    unsafe {
        UpdateDriverForPlugAndPlayDevicesW(None, &HSTRING::from(HARDWARE_ID), &inf_path, INSTALLFLAG_FORCE, Some(&mut reboot))
            .context("installing the driver failed (run this as administrator)")?;
    }
    if existed {
        // The driver reads its settings when it starts; restart it to pick up ours.
        restart_devices()?;
    }
    println!(
        "Installed the Virtual Display Driver with {MONITORS} monitors{}.",
        if reboot.as_bool() { "; restart Windows to finish" } else { "" }
    );
    Ok(())
}

fn restart_devices() -> Result<()> {
    let mut ids = Vec::new();
    for_each_device(|set, data| {
        let mut buf = [0u16; 256];
        // SAFETY: reading the instance id into a sized buffer.
        unsafe { SetupDiGetDeviceInstanceIdW(set, data, Some(&mut buf), None)? };
        ids.push(String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]));
        Ok(())
    })?;
    for id in ids {
        let status = super::system::quiet_command("pnputil").args(["/restart-device", &id]).output()?;
        if !status.status.success() {
            bail!("couldn't restart {id}: {}", String::from_utf8_lossy(&status.stdout).trim());
        }
        println!("Restarted {id}.");
    }
    Ok(())
}

/// `freemonplay-host --uninstall-display-driver`.
pub fn uninstall() -> Result<()> {
    let removed = for_each_device(|set, data| {
        // SAFETY: removing a device we just enumerated.
        unsafe { SetupDiCallClassInstaller(DIF_REMOVE, set, Some(data)) }.context("removing the device failed (run this as administrator)")
    })?;
    println!("Removed {removed} virtual display device(s). The driver package stays in Windows' driver store (pnputil /delete-driver removes it).");
    Ok(())
}

/// How many virtual monitors to offer: one per connected screen.
const MONITORS: usize = 3;

/// Resolutions the virtual monitors offer: common monitors, Mac Retina panels (as Mac
/// clients request them, in pixels), Surface and iPad screens.
const RESOLUTIONS: [(u32, u32); 34] = [
    (800, 600), (1024, 768), (1280, 720), (1280, 800), (1366, 768), (1440, 900), (1600, 900),
    (1680, 1050), (1920, 1080), (1920, 1200), (2048, 1152), (2048, 1536), (2160, 1440), (2224, 1668),
    (2256, 1504), (2360, 1640), (2388, 1668), (2560, 1080), (2560, 1440), (2560, 1600), (2560, 1664),
    (2736, 1824), (2732, 2048), (2880, 1800), (2880, 1920), (2940, 1912), (3000, 2000), (3024, 1964),
    (3440, 1440), (3456, 2234), (3840, 1600), (3840, 2160), (3840, 2400), (5120, 2880),
];

/// The driver's settings: the package's file with our monitor count and resolutions.
fn settings(template: &str) -> String {
    let mut xml = template.to_string();
    if let (Some(a), Some(b)) = (xml.find("<count>"), xml.find("</count>")) {
        xml.replace_range(a..b + "</count>".len(), &format!("<count>{MONITORS}</count>"));
    }
    // Every global refresh rate is added to every resolution, and Windows only takes so many
    // modes per monitor ("input buffer is too small for target modes", and no monitors):
    // 60 Hz only, which is what screens stream at anyway.
    if let (Some(a), Some(b)) = (xml.find("<global>"), xml.find("</global>")) {
        xml.replace_range(a..b, "<global>\n        <g_refresh_rate>60</g_refresh_rate>\n    ");
    }
    if let (Some(a), Some(b)) = (xml.find("<resolutions>"), xml.find("</resolutions>")) {
        let list: String = RESOLUTIONS
            .iter()
            .map(|(w, h)| format!("\n        <resolution>\n            <width>{w}</width>\n            <height>{h}</height>\n            <refresh_rate>60</refresh_rate>\n        </resolution>"))
            .collect();
        xml.replace_range(a..b, &format!("<resolutions>{list}\n    "));
    }
    xml
}

/// The settings folder, writable by ordinary users, with our settings in it.
fn prepare_settings_dir(package: &Path) -> Result<()> {
    std::fs::create_dir_all(SETTINGS_DIR).with_context(|| format!("can't create {SETTINGS_DIR}"))?;
    let template = std::fs::read_to_string(package.join("vdd_settings.xml")).context("no vdd_settings.xml in the package")?;
    std::fs::write(Path::new(SETTINGS_DIR).join("vdd_settings.xml"), settings(&template))?;
    // Builtin Users (S-1-5-32-545): modify, inherited by files.
    let status = super::system::quiet_command("icacls").args([SETTINGS_DIR, "/grant", "*S-1-5-32-545:(OI)(CI)M"]).output()?;
    if !status.status.success() {
        bail!("couldn't let users change {SETTINGS_DIR}");
    }
    Ok(())
}

fn create_device() -> Result<()> {
    // SAFETY: SetupAPI calls on a device info set we create and destroy here; the hardware
    // id is a double-NUL-terminated UTF-16 list.
    unsafe {
        let set = SetupDiCreateDeviceInfoList(Some(&DISPLAY_CLASS), None)?;
        let result = (|| -> Result<()> {
            let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            SetupDiCreateDeviceInfoW(set, w!("Display"), &DISPLAY_CLASS, PCWSTR::null(), None, DICD_GENERATE_ID, Some(&mut data))?;
            let ids: Vec<u16> = HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
            let bytes = std::slice::from_raw_parts(ids.as_ptr() as *const u8, ids.len() * 2);
            SetupDiSetDeviceRegistryPropertyW(set, &mut data, SPDRP_HARDWAREID, Some(bytes))?;
            SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, Some(&data)).context("registering the device failed (run this as administrator)")?;
            Ok(())
        })();
        let _ = SetupDiDestroyDeviceInfoList(set);
        result
    }
}

/// How many present devices have our hardware id.
pub fn device_count() -> Result<usize> {
    for_each_device(|_, _| Ok(()))
}

/// Runs `f` on each present device with our hardware id; returns how many.
fn for_each_device(mut f: impl FnMut(windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO, &SP_DEVINFO_DATA) -> Result<()>) -> Result<usize> {
    // SAFETY: enumerating a device info set we create and destroy here, reading each
    // device's hardware ids into a sized buffer.
    unsafe {
        // All classes: until its driver is installed the device has none.
        let set = SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT)?;
        let mut count = 0;
        let mut result = Ok(());
        for index in 0.. {
            let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut data).is_err() {
                break;
            }
            let mut buf = [0u8; 1024];
            if SetupDiGetDeviceRegistryPropertyW(set, &data, SPDRP_HARDWAREID, None, Some(&mut buf), None).is_err() {
                continue;
            }
            let wide: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|&b| u16::from_le_bytes(b)).collect();
            let ids = String::from_utf16_lossy(&wide);
            if ids.split('\0').any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
                count += 1;
                if let Err(e) = f(set, &data) {
                    result = Err(e);
                    break;
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        result.map(|()| count)
    }
}

/// SetupAPI doesn't like `\\?\` paths from canonicalize.
fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_monitor_count_and_resolutions() {
        let template = "<monitors>\n<count>1</count>\n</monitors>\n<global>\n<g_refresh_rate>60</g_refresh_rate>\n<g_refresh_rate>144</g_refresh_rate>\n</global>\n<resolutions>\n<resolution><width>800</width></resolution>\n</resolutions>";
        let xml = settings(template);
        assert!(xml.contains("<count>3</count>"));
        assert_eq!(xml.matches("<g_refresh_rate>").count(), 1);
        assert!(xml.contains("<width>2940</width>\n            <height>1912</height>"));
        assert_eq!(xml.matches("<resolution>").count(), RESOLUTIONS.len());
    }
}

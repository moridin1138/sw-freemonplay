//! Virtual displays on Windows, through the Virtual Display Driver (see driver.rs).
//!
//! The installer sets the driver up with a few virtual monitors and a broad list of
//! resolutions (driver.rs). The driver can't be reconfigured while running: its reload
//! command blocks its only control thread when monitors are in use. So for each screen that
//! connects we switch Windows to Extend (a new monitor otherwise often comes up duplicating
//! the main screen), take a free virtual monitor and give it the screen's resolution (or
//! the closest listed one of the same shape) and a place beside the desktop. When the screen
//! leaves, that monitor's display path is removed (Windows' display configuration API, as
//! Win+P uses), so nothing lingers. None of this needs administrator rights.
//!
//! Without the driver, the primary monitor is shared (mirrored) instead.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::DisplayRequest;
use windows::Win32::Foundation::{LPARAM, POINT, POINTL, RECT};
use windows::Win32::Graphics::Gdi::{
    CDS_NORESET, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    DM_POSITION, ENUM_REGISTRY_SETTINGS, EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsW,
    GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
use windows::core::{BOOL, HSTRING, PCWSTR};

use super::driver;

/// How long Windows may take to bring a virtual monitor onto the desktop.
const SETTLE_WAIT: Duration = Duration::from_secs(5);

/// Monitors in use, and where each sat on the desktop last time (detaching forgets it).
#[derive(Default)]
struct Manager {
    in_use: HashSet<String>,
    positions: HashMap<String, (i32, i32)>,
}

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);

/// A monitor the client fills: one of the driver's virtual monitors, or (without the
/// driver) the primary monitor.
pub struct VirtualDisplay {
    pub(super) monitor: HMONITOR,
    /// Desktop pixels.
    pub(super) rect: RECT,
    /// The virtual monitor's device name (e.g. `\\.\DISPLAY3`), to detach it afterwards.
    device: Option<String>,
}

// SAFETY: an HMONITOR is a plain handle, usable from any thread.
unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

impl VirtualDisplay {
    pub fn create(name: &str, _serial: u32, req: &DisplayRequest) -> Result<Self> {
        if driver_available() {
            match attach_virtual(req) {
                Ok(display) => {
                    let (w, h) = (display.rect.right - display.rect.left, display.rect.bottom - display.rect.top);
                    tracing::info!("{name}: extended the desktop onto a virtual {w}x{h} monitor");
                    return Ok(display);
                }
                Err(e) => tracing::warn!("{name}: couldn't set up a virtual monitor ({e:#}); sharing the primary monitor instead"),
            }
        } else {
            tracing::warn!(
                "{name}: the Virtual Display Driver isn't installed, so this screen mirrors the primary monitor. \
                 To extend instead, run freemonplay-host --install-display-driver <folder> as administrator once"
            );
        }
        primary()
    }

    pub fn id(&self) -> u32 {
        self.monitor.0 as usize as u32
    }

    /// The monitor handle as Windows has it now: handles can change while Windows settles
    /// a new topology (just after the driver reloads).
    pub(super) fn current_monitor(&self) -> HMONITOR {
        self.device.as_deref().and_then(find_monitor).map_or(self.monitor, |(m, _)| m)
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        let Some(device) = self.device.take() else { return };
        let mut manager = MANAGER.lock().unwrap();
        let manager = manager.get_or_insert_with(Manager::default);
        manager.positions.insert(device.clone(), (self.rect.left, self.rect.top));
        if let Err(e) = ccd::remove_virtual(Some(&device)) {
            tracing::warn!("couldn't remove the virtual monitor {device}: {e:#}");
        }
        manager.in_use.remove(&device);
        tracing::info!("removed the virtual monitor {device}");
    }
}

fn primary() -> Result<VirtualDisplay> {
    // SAFETY: plain queries with a valid out struct.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(monitor, &mut info).ok().context("no primary monitor")?;
        Ok(VirtualDisplay { monitor, rect: info.rcMonitor, device: None })
    }
}

/// Whether the Virtual Display Driver is installed (screens extend rather than mirror).
pub fn driver_available() -> bool {
    Path::new(driver::SETTINGS_DIR).join("vdd_settings.xml").exists() && driver::device_count().unwrap_or(0) > 0
}

/// At startup: hide any virtual monitor left on the desktop (a crash, or Windows bringing it
/// up duplicating the main screen after the driver was installed).
pub fn hide_leftovers() {
    match ccd::remove_virtual(None) {
        Ok(true) => tracing::info!("removed a leftover virtual monitor from the desktop"),
        Ok(false) => {}
        Err(e) => tracing::debug!("checking for leftover virtual monitors: {e:#}"),
    }
}

fn attach_virtual(req: &DisplayRequest) -> Result<VirtualDisplay> {
    log_displays();
    let mut guard = MANAGER.lock().unwrap();
    let manager = guard.get_or_insert_with(Manager::default);

    // Extend the desktop onto the virtual monitors (a new monitor otherwise often duplicates
    // the main screen), then take a free one.
    let deadline = Instant::now() + SETTLE_WAIT;
    let device = loop {
        if let Err(e) = ccd::extend() {
            tracing::debug!("switching to Extend: {e:#}");
        }
        if let Some(device) = ccd::virtual_sources().into_iter().find(|d| !manager.in_use.contains(d)) {
            break device;
        }
        if Instant::now() > deadline {
            log_displays();
            ccd::log_paths();
            remove_unused(manager);
            bail!("no free virtual monitor (all {} in use?)", manager.in_use.len());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // The installer gave the monitors a broad list of resolutions; use the screen's own, or
    // the closest of the same shape (the stream is scaled to fit either way).
    let (width, height) = best_mode(&modes(&device), req.width, req.height).context("the virtual monitor lists no modes")?;
    if (width, height) != (req.width, req.height) {
        tracing::info!("{}x{} isn't on the virtual monitor's list; using {width}x{height}", req.width, req.height);
    }
    let position = manager.positions.get(&device).copied().or_else(|| saved_position(&device)).unwrap_or_else(right_of_desktop);
    let attached = attach(&device, width, height, req.refresh_hz, position)
        .and_then(|()| find_monitor(&device).context("the virtual monitor didn't appear on the desktop"));
    // Only this screen's monitor stays on the desktop.
    let (monitor, rect) = match attached {
        Ok(found) => {
            manager.in_use.insert(device.clone());
            remove_unused(manager);
            // Text and windows at the size the screen's own display would show them.
            let scale = scale_for(req);
            match ccd::set_scale(&device, scale) {
                Ok(()) => tracing::info!("{device}: display scale {scale}%"),
                Err(e) => tracing::warn!("{device}: couldn't set the display scale to {scale}%: {e:#}"),
            }
            found
        }
        Err(e) => {
            remove_unused(manager);
            return Err(e);
        }
    };
    Ok(VirtualDisplay { monitor, rect, device: Some(device) })
}

/// The modes a monitor lists, largest first, without duplicates.
fn modes(device: &str) -> Vec<(u32, u32)> {
    let name = HSTRING::from(device);
    let mut out = Vec::new();
    for i in 0.. {
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        // SAFETY: enumerating modes into a sized struct.
        if !unsafe { EnumDisplaySettingsW(&name, windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE(i), &mut mode) }.as_bool() {
            break;
        }
        let size = (mode.dmPelsWidth, mode.dmPelsHeight);
        if !out.contains(&size) {
            out.push(size);
        }
    }
    out.sort_by_key(|&(w, h)| std::cmp::Reverse(w as u64 * h as u64));
    out
}

/// The best of `modes` for a `width` x `height` screen: exactly that, else the largest of
/// the same shape that fits, else the largest that fits, else the smallest.
fn best_mode(modes: &[(u32, u32)], width: u32, height: u32) -> Option<(u32, u32)> {
    let aspect = width as f64 / height as f64;
    let fits = |&&(w, h): &&(u32, u32)| w <= width && h <= height;
    let same_shape = |&&(w, h): &&(u32, u32)| ((w as f64 / h as f64) - aspect).abs() < 0.02;
    modes
        .iter()
        .find(|&&m| m == (width, height))
        .or_else(|| modes.iter().filter(fits).find(same_shape))
        .or_else(|| modes.iter().find(fits))
        .or_else(|| modes.last())
        .copied()
}

/// The display scale (percent) for a screen: 200% for a HiDPI one (a Retina Mac, or a
/// Windows PC at 150% or more) so things appear the size they do there, else 100%.
fn scale_for(req: &DisplayRequest) -> u32 {
    if req.hidpi { 200 } else { 100 }
}

/// Removes virtual monitors that no session is using from the desktop.
fn remove_unused(manager: &Manager) {
    for device in ccd::virtual_sources() {
        if !manager.in_use.contains(&device) {
            let _ = ccd::remove_virtual(Some(&device));
        }
    }
}

/// One of Windows' display outputs.
struct Display {
    device: String,
    adapter: String,
    monitor_id: String,
    attached: bool,
}

impl Display {
    /// The driver's adapter calls itself a virtual display; its monitors' EDID says MTT.
    fn is_virtual(&self) -> bool {
        self.adapter.to_lowercase().contains("virtual display") || self.monitor_id.to_uppercase().contains(r"\MTT")
    }
}

fn log_displays() {
    for d in displays() {
        tracing::info!(
            "display {}: {} | monitor {} | {}{}",
            d.device,
            d.adapter,
            if d.monitor_id.is_empty() { "-" } else { &d.monitor_id },
            if d.attached { "attached" } else { "detached" },
            if d.is_virtual() { " | virtual" } else { "" }
        );
    }
}

fn displays() -> Vec<Display> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut adapter = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: enumerating display adapters into a sized struct.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut adapter, 0) }.as_bool() {
            break;
        }
        let device = wide(&adapter.DeviceName);
        let mut monitor = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: as above, for the adapter's first monitor.
        let has_monitor = unsafe { EnumDisplayDevicesW(&HSTRING::from(&device), 0, &mut monitor, 0) }.as_bool();
        out.push(Display {
            device,
            adapter: wide(&adapter.DeviceString),
            monitor_id: if has_monitor { wide(&monitor.DeviceID) } else { String::new() },
            attached: (adapter.StateFlags & windows::Win32::Graphics::Gdi::DISPLAY_DEVICE_ATTACHED_TO_DESKTOP).0 != 0,
        });
    }
    out
}

/// Attaches `device` to the desktop at `position`, with that size and refresh rate.
fn attach(device: &str, width: u32, height: u32, refresh_hz: u32, position: (i32, i32)) -> Result<()> {
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_POSITION | DM_DISPLAYFREQUENCY;
    mode.dmPelsWidth = width;
    mode.dmPelsHeight = height;
    mode.dmDisplayFrequency = refresh_hz;
    mode.Anonymous1.Anonymous2.dmPosition = POINTL { x: position.0, y: position.1 };
    if change(device, &mode).is_err() {
        // Refresh rates like 59 aren't always on offer; take the default for that size.
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_POSITION;
        change(device, &mode).with_context(|| format!("Windows refused {width}x{height} on {device}"))?;
    }
    Ok(())
}

fn change(device: &str, mode: &DEVMODEW) -> Result<()> {
    // SAFETY: stage the change for one device, then apply all staged changes.
    unsafe {
        let staged = ChangeDisplaySettingsExW(&HSTRING::from(device), Some(mode), None, CDS_UPDATEREGISTRY | CDS_NORESET, None);
        if staged != DISP_CHANGE_SUCCESSFUL {
            bail!("display change rejected ({})", staged.0);
        }
        let applied = ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None);
        if applied != DISP_CHANGE_SUCCESSFUL {
            bail!("applying the display change failed ({})", applied.0);
        }
    }
    Ok(())
}

/// Where Windows last kept this monitor, if it remembers.
fn saved_position(device: &str) -> Option<(i32, i32)> {
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: reading the saved settings into a sized struct.
    let ok = unsafe { EnumDisplaySettingsW(&HSTRING::from(device), ENUM_REGISTRY_SETTINGS, &mut mode) }.as_bool();
    // SAFETY: display devices fill the position member of the union.
    let p = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
    (ok && mode.dmPelsWidth > 0).then_some((p.x, p.y))
}

/// Just right of the current desktop, top aligned.
fn right_of_desktop() -> (i32, i32) {
    // SAFETY: plain system metric queries.
    unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN)) }
}

/// The attached monitor for a device name, and its desktop rectangle.
fn find_monitor(device: &str) -> Option<(HMONITOR, RECT)> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed below, alive for the enumeration.
        let found = unsafe { &mut *(data.0 as *mut Vec<(HMONITOR, String, RECT)>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: a sized MONITORINFOEXW.
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            found.push((monitor, wide(&info.szDevice), info.monitorInfo.rcMonitor));
        }
        BOOL(1)
    }
    let mut found: Vec<(HMONITOR, String, RECT)> = Vec::new();
    for _ in 0..20 {
        found.clear();
        // SAFETY: enumerating monitors with our callback and a pointer to `found`.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut found as *mut _ as isize)) };
        if let Some((m, _, r)) = found.iter().find(|(_, d, _)| d.eq_ignore_ascii_case(device)) {
            return Some((*m, *r));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn wide(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
}

mod ccd {
    //! Windows' display configuration API (the machinery behind Win+P): finds the virtual
    //! monitors by their EDID id and switches their display paths on and off.

    use anyhow::{Result, bail};
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_APPLY,
        SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_EXTEND, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SetDisplayConfig,
    };

    use super::wide;

    /// The driver's monitors identify themselves with this EDID id.
    const VIRTUAL_EDID: &str = "MTT1337";

    fn active() -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: sizing, then filling buffers of those sizes.
        unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).ok()?;
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).ok()?;
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            Ok((paths, modes))
        }
    }

    fn target_path(p: &DISPLAYCONFIG_PATH_INFO) -> String {
        let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: p.targetInfo.adapterId,
                id: p.targetInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: a correctly sized request packet.
        unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
        wide(&name.monitorDevicePath)
    }

    fn source_name(p: &DISPLAYCONFIG_PATH_INFO) -> String {
        let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: a correctly sized request packet.
        unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
        wide(&name.viewGdiDeviceName)
    }

    fn is_virtual(p: &DISPLAYCONFIG_PATH_INFO) -> bool {
        target_path(p).to_uppercase().contains(VIRTUAL_EDID)
    }

    /// Extend the desktop across all connected monitors (as Win+P > Extend).
    pub fn extend() -> Result<()> {
        // SAFETY: a topology change with no supplied configuration.
        let status = unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) };
        if status != 0 {
            bail!("SetDisplayConfig(Extend) failed ({status})");
        }
        Ok(())
    }

    /// GDI names (`\\.\DISPLAYn`) of active virtual monitors with a desktop of their own (not
    /// duplicating another monitor).
    pub fn virtual_sources() -> Vec<String> {
        let Ok((paths, _)) = active() else { return Vec::new() };
        paths
            .iter()
            .filter(|p| is_virtual(p))
            .filter(|p| {
                let shared = paths.iter().filter(|q| q.sourceInfo.adapterId == p.sourceInfo.adapterId && q.sourceInfo.id == p.sourceInfo.id).count();
                shared == 1
            })
            .map(source_name)
            .collect()
    }

    /// Removes virtual monitors' display paths: the one showing `device`, or all of them.
    /// Returns whether anything changed.
    pub fn remove_virtual(device: Option<&str>) -> Result<bool> {
        let (paths, modes) = active()?;
        let keep: Vec<DISPLAYCONFIG_PATH_INFO> = paths
            .iter()
            .filter(|p| !(is_virtual(p) && device.is_none_or(|d| source_name(p).eq_ignore_ascii_case(d))))
            .copied()
            .collect();
        if keep.len() == paths.len() || keep.is_empty() {
            return Ok(false);
        }
        let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE;
        // SAFETY: applying a subset of the active configuration Windows just gave us.
        let status = unsafe { SetDisplayConfig(Some(&keep), Some(&modes), flags) };
        if status != 0 {
            bail!("SetDisplayConfig failed ({status})");
        }
        Ok(true)
    }

    /// Windows' display scale steps, percent. The (undocumented, but long stable) scale
    /// requests below count in steps relative to the monitor's recommended scale, as the
    /// Settings app does.
    const SCALES: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];
    const GET_DPI_SCALE: i32 = -3;
    const SET_DPI_SCALE: i32 = -4;

    #[repr(C)]
    struct GetDpiScale {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
        min_relative: i32,
        current_relative: i32,
        max_relative: i32,
    }

    #[repr(C)]
    struct SetDpiScale {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
        relative: i32,
    }

    /// Sets the display scale of the monitor showing `device` (\\.\DISPLAYn) to the
    /// nearest step to `percent` it allows.
    pub fn set_scale(device: &str, percent: u32) -> Result<()> {
        let (paths, _) = active()?;
        let Some(path) = paths.iter().find(|p| source_name(p).eq_ignore_ascii_case(device)) else {
            bail!("no active display path for {device}");
        };
        let header = |kind: i32, size: usize| DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(kind),
            size: size as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        };
        let mut get = GetDpiScale {
            header: header(GET_DPI_SCALE, size_of::<GetDpiScale>()),
            min_relative: 0,
            current_relative: 0,
            max_relative: 0,
        };
        // SAFETY: a correctly sized request packet.
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut get.header) };
        if status != 0 {
            bail!("reading the display scale failed ({status})");
        }
        let target = relative_step(get.min_relative, get.max_relative, percent);
        if target == get.current_relative {
            return Ok(());
        }
        let set = SetDpiScale { header: header(SET_DPI_SCALE, size_of::<SetDpiScale>()), relative: target };
        // SAFETY: as above.
        let status = unsafe { DisplayConfigSetDeviceInfo(&set.header) };
        if status != 0 {
            bail!("setting the display scale failed ({status})");
        }
        Ok(())
    }

    /// The step, relative to the recommended one, closest to `percent`. `min` is the
    /// (negative) step of 100%, `max` that of the largest scale allowed.
    pub(super) fn relative_step(min: i32, max: i32, percent: u32) -> i32 {
        let recommended = -min;
        let wanted = SCALES.iter().enumerate().min_by_key(|(_, s)| s.abs_diff(percent)).map_or(0, |(i, _)| i as i32);
        (wanted - recommended).clamp(min, max)
    }

    pub fn log_paths() {
        let Ok((paths, _)) = active() else { return };
        for p in &paths {
            tracing::info!("active path: {} -> {}{}", source_name(p), target_path(p), if is_virtual(p) { " (virtual)" } else { "" });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [(u32, u32); 5] = [(3840, 2160), (2940, 1912), (2560, 1600), (1920, 1080), (1280, 800)];

    #[test]
    fn picks_the_closest_mode() {
        assert_eq!(best_mode(&MODES, 2940, 1912), Some((2940, 1912)));
        // Same shape (16:10), fits: 2560x1600 over the bigger 16:9 one.
        assert_eq!(best_mode(&MODES, 2880, 1800), Some((2560, 1600)));
        // No 16:10 fits 1600x1000 but 1280x800 does.
        assert_eq!(best_mode(&MODES, 1600, 1000), Some((1280, 800)));
        assert_eq!(best_mode(&MODES, 1000, 700), Some((1280, 800)));
        assert_eq!(best_mode(&[], 1000, 700), None);
    }

    #[test]
    fn scale_steps_are_relative_to_the_recommended_one() {
        // Recommended 150% (step 2): 100% is -2, 200% is +2.
        assert_eq!(ccd::relative_step(-2, 5, 100), -2);
        assert_eq!(ccd::relative_step(-2, 5, 200), 2);
        // Recommended 100%, but at most 175% allowed.
        assert_eq!(ccd::relative_step(0, 3, 200), 3);
    }
}

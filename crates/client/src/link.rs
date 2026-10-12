//! The network link to the host: tunes it for streaming where the OS allows, and describes
//! it (Wi-Fi band, signal, link rate) so the host can show what's limiting the stream.
//!
//! Described on Windows, macOS and Linux (NetworkManager); tuned on Windows only.
//!
//! Windows: Wi-Fi "media streaming mode" stops the adapter dozing between access point
//! beacons, which otherwise adds tens of milliseconds (and jitter) to every round trip.
//! It's released when the session ends.

use std::net::IpAddr;

use fmp_proto::NetworkInfo;

/// Holds the link tuning for the length of a session.
pub struct LinkTuning(#[allow(dead_code)] imp::Inner);

/// Tunes and describes the link used to reach `host`. Either part may be missing.
pub fn tune(host: IpAddr) -> (Option<LinkTuning>, Option<NetworkInfo>) {
    match imp::tune(host) {
        Ok((inner, info)) => {
            if let Some(info) = &info {
                tracing::info!("network: {}", info.summary);
                for tip in &info.advice {
                    tracing::info!("network tip: {tip}");
                }
            }
            (inner.map(LinkTuning), info)
        }
        Err(e) => {
            tracing::debug!("couldn't inspect the network link: {e:#}");
            (None, None)
        }
    }
}

#[cfg_attr(not(any(target_os = "windows", target_os = "macos", target_os = "linux")), allow(dead_code))]
fn wired() -> NetworkInfo {
    NetworkInfo { summary: "wired (not Wi-Fi)".into(), advice: Vec::new() }
}

/// Advice for a Wi-Fi link, from its band, signal (0 to 100) and link rate (Mbit/s).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos", target_os = "linux")), allow(dead_code))]
/// ", link 866 Mbit/s", or nothing while the rate isn't known yet (just after joining).
#[cfg_attr(not(any(target_os = "windows", target_os = "macos", target_os = "linux")), allow(dead_code))]
fn link_text(mbps: f64) -> String {
    if mbps >= 1.0 { format!(", link {mbps:.0} Mbit/s") } else { String::new() }
}

fn wifi_advice(two_point_four: bool, signal: u32, link_mbps: u32) -> Vec<String> {
    let mut advice = Vec::new();
    if two_point_four {
        advice.push("on 2.4 GHz Wi-Fi: 5 GHz (or a cable) is usually much faster and steadier".to_string());
    }
    if signal < 50 {
        advice.push(format!("weak Wi-Fi signal ({signal}%): move closer to the router, or use a cable"));
    }
    if link_mbps > 0 && link_mbps < 150 {
        advice.push(format!("slow Wi-Fi link ({link_mbps} Mbit/s): lower the host's frame rate or bitrate"));
    }
    advice
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::net::IpAddr;
    use std::ptr::null_mut;

    use anyhow::{Result, bail};
    use fmp_proto::NetworkInfo;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceIndexToLuid, ConvertInterfaceLuidToGuid, GetBestInterface,
    };
    use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows::Win32::NetworkManagement::WiFi::{
        WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces,
        WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, WlanSetInterface, wlan_interface_state_connected,
        wlan_intf_opcode_channel_number, wlan_intf_opcode_current_connection, wlan_intf_opcode_media_streaming_mode,
    };
    use windows::core::GUID;

    /// Keeps the WLAN handle open: Windows drops media streaming mode when it closes.
    pub struct Inner(HANDLE);

    // SAFETY: a WLAN client handle may be used and closed from any thread.
    unsafe impl Send for Inner {}

    impl Drop for Inner {
        fn drop(&mut self) {
            // SAFETY: closing the handle we opened.
            unsafe { WlanCloseHandle(self.0, None) };
        }
    }

    pub fn tune(host: IpAddr) -> Result<(Option<Inner>, Option<NetworkInfo>)> {
        let IpAddr::V4(v4) = host else { return Ok((None, None)) };
        // SAFETY: plain Win32 calls with valid out pointers; WLAN memory is freed below.
        unsafe {
            let mut index = 0u32;
            let status = GetBestInterface(u32::from_ne_bytes(v4.octets()), &mut index);
            if status != 0 {
                bail!("no route to the host ({status})");
            }
            let mut luid = NET_LUID_LH::default();
            ConvertInterfaceIndexToLuid(index, &mut luid).ok()?;
            let mut route_guid = GUID::zeroed();
            ConvertInterfaceLuidToGuid(&luid, &mut route_guid).ok()?;

            let (mut version, mut handle) = (0u32, HANDLE::default());
            if WlanOpenHandle(2, None, &mut version, &mut handle) != 0 {
                // No WLAN service (a desktop without Wi-Fi): the link isn't Wi-Fi.
                return Ok((None, Some(super::wired())));
            }
            let inner = Inner(handle);
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
            if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
                return Ok((None, Some(super::wired())));
            }
            let count = (*list).dwNumberOfItems as usize;
            let first = (*list).InterfaceInfo.as_ptr();
            let wifi: Option<WLAN_INTERFACE_INFO> = (0..count)
                .map(|i| *first.add(i))
                .find(|i| i.InterfaceGuid == route_guid && i.isState == wlan_interface_state_connected);
            WlanFreeMemory(list as *const c_void);
            let Some(wifi) = wifi else { return Ok((None, Some(super::wired()))) };
            let guid = wifi.InterfaceGuid;

            let enable: i32 = 1;
            let streaming = WlanSetInterface(
                handle,
                &guid,
                wlan_intf_opcode_media_streaming_mode,
                size_of::<i32>() as u32,
                &enable as *const i32 as *const c_void,
                None,
            ) == 0;

            let mut size = 0u32;
            let mut data: *mut c_void = null_mut();
            let (mut signal, mut link_mbps, mut phy) = (0u32, 0u32, 0i32);
            if WlanQueryInterface(handle, &guid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) == 0
                && !data.is_null()
            {
                let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                let assoc = &attrs.wlanAssociationAttributes;
                signal = assoc.wlanSignalQuality;
                link_mbps = assoc.ulTxRate.max(assoc.ulRxRate) / 1000;
                phy = assoc.dot11PhyType.0;
                WlanFreeMemory(data);
            }
            let mut channel = 0u32;
            data = null_mut();
            if WlanQueryInterface(handle, &guid, wlan_intf_opcode_channel_number, None, &mut size, &mut data, None) == 0
                && !data.is_null()
            {
                channel = *(data as *const u32);
                WlanFreeMemory(data);
            }

            let generation = match phy {
                7 => "Wi-Fi 4",
                8 => "Wi-Fi 5",
                10 => "Wi-Fi 6",
                11 => "Wi-Fi 7",
                4..=6 => "older Wi-Fi",
                _ => "Wi-Fi",
            };
            // Wi-Fi 6E's 6 GHz channels reuse low numbers, so only call it 2.4 GHz when the
            // standard rules 6 GHz out.
            let two_point_four = (1..=14).contains(&channel) && phy < 10;
            let band = match channel {
                0 => String::new(),
                1..=14 if phy < 10 => ", 2.4 GHz".into(),
                1..=14 => format!(", channel {channel}"),
                _ => ", 5 GHz or higher".into(),
            };
            let tuned = if streaming { ", low-latency mode on" } else { "" };
            let summary = format!("{generation}{band}, signal {signal}%{}{tuned}", super::link_text(link_mbps as f64));
            let advice = super::wifi_advice(two_point_four, signal, link_mbps);
            Ok((streaming.then_some(inner), Some(NetworkInfo { summary, advice })))
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! CoreWLAN, for the Wi-Fi interface, after checking the host is reached through it.
    //! macOS has no streaming mode to switch on.

    use std::net::IpAddr;
    use std::process::Command;

    use anyhow::{Context, Result};
    use fmp_proto::NetworkInfo;
    use objc2_core_wlan::{CWChannelBand, CWPHYMode, CWWiFiClient};

    pub struct Inner;

    pub fn tune(host: IpAddr) -> Result<(Option<Inner>, Option<NetworkInfo>)> {
        let route = route_interface(host)?;
        // SAFETY: read-only CoreWLAN queries.
        let wifi = unsafe {
            CWWiFiClient::sharedWiFiClient().interface().filter(|i| {
                i.serviceActive() && i.interfaceName().is_some_and(|n| n.to_string() == route)
            })
        };
        let Some(wifi) = wifi else { return Ok((None, Some(super::wired()))) };
        // SAFETY: as above.
        let (channel, phy, rssi, rate) =
            unsafe { (wifi.wlanChannel(), wifi.activePHYMode(), wifi.rssiValue(), wifi.transmitRate()) };
        let generation = match phy {
            CWPHYMode::Mode11n => "Wi-Fi 4",
            CWPHYMode::Mode11ac => "Wi-Fi 5",
            CWPHYMode::Mode11ax => "Wi-Fi 6",
            CWPHYMode::Mode11a | CWPHYMode::Mode11b | CWPHYMode::Mode11g => "older Wi-Fi",
            _ => "Wi-Fi",
        };
        // SAFETY: as above.
        let band = channel.map(|c| unsafe { c.channelBand() });
        let band_name = match band {
            Some(CWChannelBand::Band2GHz) => ", 2.4 GHz",
            Some(CWChannelBand::Band5GHz) => ", 5 GHz",
            Some(CWChannelBand::Band6GHz) => ", 6 GHz",
            _ => "",
        };
        // RSSI as a rough 0 to 100 quality, as Windows reports it (-100 dBm is 0, -50 is 100).
        let signal = ((rssi + 100) * 2).clamp(0, 100) as u32;
        let summary = format!("{generation}{band_name}, signal {rssi} dBm{}", super::link_text(rate));
        let advice = super::wifi_advice(band == Some(CWChannelBand::Band2GHz), signal, rate as u32);
        Ok((None, Some(NetworkInfo { summary, advice })))
    }

    /// The interface the route to `host` uses (e.g. "en0").
    fn route_interface(host: IpAddr) -> Result<String> {
        let out = Command::new("/sbin/route").args(["-n", "get", &host.to_string()]).output()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.trim().strip_prefix("interface:").map(|i| i.trim().to_string()))
            .context("no route to the host")
    }
}

#[cfg(target_os = "linux")]
mod imp {
    //! NetworkManager over D-Bus, for the device that routes to the host.

    use std::net::IpAddr;

    use anyhow::{Context, Result};
    use fmp_proto::NetworkInfo;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::OwnedObjectPath;

    pub struct Inner;

    const NM: &str = "org.freedesktop.NetworkManager";
    const WIFI_DEVICE: u32 = 2;

    pub fn tune(host: IpAddr) -> Result<(Option<Inner>, Option<NetworkInfo>)> {
        let IpAddr::V4(v4) = host else { return Ok((None, None)) };
        let iface = route_interface(v4.octets())?;
        let bus = Connection::system().context("no D-Bus system bus")?;
        let nm = Proxy::new(&bus, NM, "/org/freedesktop/NetworkManager", NM)?;
        let device: OwnedObjectPath = nm.call("GetDeviceByIpIface", &(iface.as_str(),)).context("not managed by NetworkManager")?;
        let dev = Proxy::new(&bus, NM, device.as_str(), "org.freedesktop.NetworkManager.Device")?;
        if dev.get_property::<u32>("DeviceType")? != WIFI_DEVICE {
            return Ok((None, Some(super::wired())));
        }
        let wireless = Proxy::new(&bus, NM, device.as_str(), "org.freedesktop.NetworkManager.Device.Wireless")?;
        let link_mbps = wireless.get_property::<u32>("Bitrate")? / 1000;
        let ap_path: OwnedObjectPath = wireless.get_property("ActiveAccessPoint")?;
        let ap = Proxy::new(&bus, NM, ap_path.as_str(), "org.freedesktop.NetworkManager.AccessPoint")?;
        let mhz = ap.get_property::<u32>("Frequency")?;
        let signal = ap.get_property::<u8>("Strength")? as u32;
        let band = match mhz {
            2400..=2500 => ", 2.4 GHz",
            5000..=5900 => ", 5 GHz",
            5925..=7125 => ", 6 GHz",
            _ => "",
        };
        let summary = format!("Wi-Fi{band}, signal {signal}%{}", super::link_text(link_mbps as f64));
        let advice = super::wifi_advice((2400..=2500).contains(&mhz), signal, link_mbps);
        Ok((None, Some(NetworkInfo { summary, advice })))
    }

    /// The interface whose route to `dest` is most specific, from /proc/net/route.
    fn route_interface(dest: [u8; 4]) -> Result<String> {
        let table = std::fs::read_to_string("/proc/net/route")?;
        let dest = u32::from_ne_bytes(dest);
        table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                let net = u32::from_str_radix(f.get(1)?, 16).ok()?;
                let mask = u32::from_str_radix(f.get(7)?, 16).ok()?;
                (dest & mask == net).then(|| (mask.count_ones(), f[0].to_string()))
            })
            .max_by_key(|(bits, _)| *bits)
            .map(|(_, iface)| iface)
            .context("no route to the host")
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod imp {
    use std::net::IpAddr;

    use anyhow::Result;
    use fmp_proto::NetworkInfo;

    pub struct Inner;

    pub fn tune(_host: IpAddr) -> Result<(Option<Inner>, Option<NetworkInfo>)> {
        Ok((None, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advice_for_poor_wifi() {
        assert!(wifi_advice(false, 80, 866).is_empty());
        let tips = wifi_advice(true, 35, 72);
        assert_eq!(tips.len(), 3);
        assert!(tips[1].contains("35%"));
    }
}

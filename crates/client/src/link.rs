//! The network link to the host: tunes it for streaming where the OS allows, and describes
//! it (Wi-Fi band, signal, link rate) so the host can show what's limiting the stream.
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

/// Advice for a Wi-Fi link, from its band, signal (0 to 100) and link rate (Mbit/s).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
                return Ok((None, Some(wired())));
            }
            let inner = Inner(handle);
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
            if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
                return Ok((None, Some(wired())));
            }
            let count = (*list).dwNumberOfItems as usize;
            let first = (*list).InterfaceInfo.as_ptr();
            let wifi: Option<WLAN_INTERFACE_INFO> = (0..count)
                .map(|i| *first.add(i))
                .find(|i| i.InterfaceGuid == route_guid && i.isState == wlan_interface_state_connected);
            WlanFreeMemory(list as *const c_void);
            let Some(wifi) = wifi else { return Ok((None, Some(wired()))) };
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
            let summary = format!("{generation}{band}, signal {signal}%, link {link_mbps} Mbit/s{tuned}");
            let advice = super::wifi_advice(two_point_four, signal, link_mbps);
            Ok((streaming.then_some(inner), Some(NetworkInfo { summary, advice })))
        }
    }

    fn wired() -> NetworkInfo {
        NetworkInfo { summary: "wired (not Wi-Fi)".into(), advice: Vec::new() }
    }
}

#[cfg(not(target_os = "windows"))]
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

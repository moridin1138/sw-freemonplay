//! This Mac's own Wi-Fi link, for the menu: on Wi-Fi the host is half of every round trip,
//! and a 2.4 GHz link or AWDL (the radio link behind AirDrop, Handoff and Universal
//! Control) can easily be what limits streaming.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fmp_proto::{NetworkInfo, NetworkTip, Signal, WifiBand, WifiLink};
use objc2_core_wlan::{CWChannelBand, CWChannelWidth, CWPHYMode, CWWiFiClient};

/// The menu refreshes often; the answer changes rarely.
const CACHE_FOR: Duration = Duration::from_secs(10);
static CACHE: Mutex<Option<(Instant, Option<NetworkInfo>)>> = Mutex::new(None);

/// `None` when this Mac isn't using Wi-Fi.
pub fn describe() -> Option<NetworkInfo> {
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, info)) = &*cache
        && at.elapsed() < CACHE_FOR
    {
        return info.clone();
    }
    let info = wifi();
    *cache = Some((Instant::now(), info.clone()));
    info
}

fn wifi() -> Option<NetworkInfo> {
    // SAFETY: read-only CoreWLAN queries.
    let (band, width, number, phy, rssi, rate) = unsafe {
        let interface = CWWiFiClient::sharedWiFiClient().interface()?;
        if !interface.serviceActive() {
            return None;
        }
        let channel = interface.wlanChannel()?;
        (
            channel.channelBand(),
            channel.channelWidth(),
            channel.channelNumber(),
            interface.activePHYMode(),
            interface.rssiValue(),
            interface.transmitRate(),
        )
    };
    let generation = match phy {
        CWPHYMode::Mode11n => 4,
        CWPHYMode::Mode11ac => 5,
        CWPHYMode::Mode11ax => 6,
        CWPHYMode::Mode11a | CWPHYMode::Mode11b | CWPHYMode::Mode11g => 3,
        _ => 0,
    };
    let band = match band {
        CWChannelBand::Band2GHz => WifiBand::TwoPointFour,
        CWChannelBand::Band5GHz => WifiBand::Five,
        CWChannelBand::Band6GHz => WifiBand::Six,
        _ => WifiBand::Unknown,
    };
    let width_mhz = match width {
        CWChannelWidth::Width20MHz => 20,
        CWChannelWidth::Width40MHz => 40,
        CWChannelWidth::Width80MHz => 80,
        CWChannelWidth::Width160MHz => 160,
        _ => 0,
    };
    let rssi = rssi as i32;
    let link_mbps = rate.max(0.0) as u32;
    let wifi = WifiLink {
        generation,
        band,
        channel: number as u32,
        width_mhz,
        signal: Signal::Dbm(rssi),
        link_mbps,
        low_latency: false,
    };
    let mut tips = fmp_proto::wifi_tips(band, fmp_proto::rssi_quality(rssi), link_mbps);
    if awdl_active() {
        tips.push(NetworkTip::Awdl);
    }
    Some(NetworkInfo { wifi: Some(wifi), tips })
}

/// Whether the AWDL interface is up and in use (it hops the radio between channels).
fn awdl_active() -> bool {
    Command::new("/sbin/ifconfig")
        .arg("awdl0")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("status: active"))
}

#[cfg(test)]
mod tests {
    /// Prints this Mac's link (CI machines are usually wired, so it can't assert much).
    #[test]
    fn describes_this_mac() {
        if let Some(info) = super::describe() {
            println!("{info}");
            assert!(info.wifi.is_some());
        }
    }
}

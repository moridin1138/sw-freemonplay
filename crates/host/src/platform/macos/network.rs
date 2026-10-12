//! This Mac's own Wi-Fi link, for the menu: on Wi-Fi the host is half of every round trip,
//! and a 2.4 GHz link or AWDL (the radio link behind AirDrop, Handoff and Universal
//! Control) can easily be what limits streaming.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fmp_proto::NetworkInfo;
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
        CWPHYMode::Mode11n => "Wi-Fi 4",
        CWPHYMode::Mode11ac => "Wi-Fi 5",
        CWPHYMode::Mode11ax => "Wi-Fi 6",
        CWPHYMode::Mode11a | CWPHYMode::Mode11b | CWPHYMode::Mode11g => "older Wi-Fi",
        _ => "Wi-Fi",
    };
    let band_name = match band {
        CWChannelBand::Band2GHz => "2.4 GHz",
        CWChannelBand::Band5GHz => "5 GHz",
        CWChannelBand::Band6GHz => "6 GHz",
        _ => "unknown band",
    };
    let mhz = match width {
        CWChannelWidth::Width20MHz => 20,
        CWChannelWidth::Width40MHz => 40,
        CWChannelWidth::Width80MHz => 80,
        CWChannelWidth::Width160MHz => 160,
        _ => 0,
    };
    let width = if mhz > 0 { format!(" {mhz} MHz") } else { String::new() };
    let summary =
        format!(
            "{generation}, {band_name} (channel {number}{width}), signal {rssi} dBm{}",
            if rate >= 1.0 { format!(", link {rate:.0} Mbit/s") } else { String::new() }
        );

    let mut advice = Vec::new();
    if band == CWChannelBand::Band2GHz {
        advice.push("this Mac is on 2.4 GHz Wi-Fi: 5 GHz (or a cable) is usually much faster and steadier".to_string());
    }
    if rssi < -70 {
        advice.push(format!("weak Wi-Fi signal ({rssi} dBm): move closer to the router, or use a cable"));
    }
    if awdl_active() {
        advice.push(
            "AirDrop, Handoff or Universal Control is keeping a second Wi-Fi link (AWDL) busy, which \
             can cause latency spikes; turning them off while streaming may help"
                .to_string(),
        );
    }
    Some(NetworkInfo { summary, advice })
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
            println!("{}: {:?}", info.summary, info.advice);
            assert!(info.summary.contains("Mbit/s"));
        }
    }
}

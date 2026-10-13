//! A network description (facts from either computer) in this app's language.

use fmp_proto::{NetworkInfo, NetworkTip, Signal, WifiBand};
use rust_i18n::t;

/// "Wi-Fi 6, 5 GHz, channel 36 (80 MHz), signal -46 dBm, link 1020 Mbit/s", or "Wired".
pub fn summary(info: &NetworkInfo) -> String {
    let Some(w) = &info.wifi else { return t!("host.net.wired").into_owned() };
    let mut parts = vec![match w.generation {
        0 => t!("host.net.wifi").into_owned(),
        3 => t!("host.net.wifi_older").into_owned(),
        n => t!("host.net.wifi_generation", n = n).into_owned(),
    }];
    match w.band {
        WifiBand::Unknown => {}
        WifiBand::TwoPointFour => parts.push(t!("host.net.band_2_4").into_owned()),
        WifiBand::Five => parts.push("5 GHz".into()),
        WifiBand::Six => parts.push("6 GHz".into()),
        WifiBand::FiveOrHigher => parts.push(t!("host.net.band_5_or_higher").into_owned()),
    }
    if w.channel > 0 {
        let mut channel = t!("host.net.channel", n = w.channel).into_owned();
        if w.width_mhz > 0 {
            channel.push_str(&format!(" ({} MHz)", w.width_mhz));
        }
        parts.push(channel);
    }
    parts.push(match w.signal {
        Signal::Dbm(dbm) => t!("host.net.signal_dbm", dbm = dbm).into_owned(),
        Signal::Percent(percent) => t!("host.net.signal_percent", percent = percent).into_owned(),
    });
    if w.link_mbps > 0 {
        parts.push(t!("host.net.link", mbps = w.link_mbps).into_owned());
    }
    if w.low_latency {
        parts.push(t!("host.net.low_latency").into_owned());
    }
    parts.join(", ")
}

/// Each tip as a sentence.
pub fn tips(info: &NetworkInfo) -> Vec<String> {
    info.tips
        .iter()
        .map(|tip| match tip {
            NetworkTip::TwoPointFour => t!("host.net.tip_2_4"),
            NetworkTip::WeakSignal => t!("host.net.tip_weak"),
            NetworkTip::SlowLink => t!("host.net.tip_slow"),
            NetworkTip::Awdl => t!("host.net.tip_awdl"),
        })
        .map(|tip| tip.into_owned())
        .collect()
}

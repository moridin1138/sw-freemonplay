//! Live list of connected screens, for the menu bar app (and anything else that wants to
//! show or control sessions).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fmp_proto::{ClientStats, Codec, Edge, NetworkInfo};

use rust_i18n::t;

use crate::platform::Activity;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: u64,
    /// The client's stable id (per computer, shared by its monitors).
    pub client_id: String,
    pub client_name: String,
    /// Keyboard and mouse mode, if the session offers it.
    pub kvm: Option<KvmRole>,
    pub addr: SocketAddr,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    pub encoder: &'static str,
    pub fps: f32,
    pub mbps: f32,
    /// Capture to sent on this Mac (encode + queueing), ms.
    pub host_ms: f32,
    pub rtt_ms: f32,
    /// Adaptive bitrate: the current target and the ceiling, Mbit/s.
    pub target_mbps: f32,
    pub max_mbps: f32,
    /// Share of packets lost recently, percent.
    pub loss_pct: f32,
    /// The client's description of its network link, if it sent one.
    pub network: Option<NetworkInfo>,
    /// Latest report from the client, once one has arrived.
    pub client: Option<ClientStats>,
}

impl SessionInfo {
    /// Estimated capture-to-shown latency: host + half the round trip + client. Doesn't
    /// include capture's own delay or waiting for the client display's next refresh.
    pub fn latency_ms(&self) -> Option<f32> {
        self.client.map(|c| self.host_ms + self.rtt_ms / 2.0 + c.display_ms)
    }

    /// One line for the menu: who, size, codec and live stats.
    pub fn title(&self) -> String {
        let rate = if self.fps > 0.0 { format!(", {:.0} fps, {:.1} Mbit/s", self.fps, self.mbps) } else { String::new() };
        let latency = self.latency_ms().map(|ms| format!(", ~{ms:.0} ms")).unwrap_or_default();
        format!("{}: {}×{} {}{rate}{latency}", self.client_name, self.width, self.height, self.codec)
    }

    /// The session's details for its submenu; `None` is a separator.
    pub fn details(&self) -> Vec<Option<String>> {
        let mut lines = vec![
            Some(t!("host.detail.address", address = self.addr.ip().to_canonical()).into_owned()),
            Some(t!("host.detail.encoder", encoder = self.encoder).into_owned()),
            Some(if self.target_mbps < self.max_mbps - 0.05 {
                let (now, max) = (format!("{:.1}", self.target_mbps), format!("{:.0}", self.max_mbps));
                t!("host.detail.bitrate_lowered", now = now, max = max).into_owned()
            } else {
                t!("host.detail.bitrate", max = format!("{:.0}", self.max_mbps)).into_owned()
            }),
        ];
        if self.loss_pct >= 0.1 {
            lines.push(Some(t!("host.detail.loss", percent = format!("{:.1}", self.loss_pct)).into_owned()));
        }
        if let Some(network) = &self.network {
            lines.push(Some(t!("host.detail.network", summary = network.summary).into_owned()));
            lines.extend(network.advice.iter().map(|tip| Some(format!("    {}", t!("host.menu.tip", tip = tip)))));
        }
        lines.push(None);
        match (self.latency_ms(), self.client) {
            (Some(total), Some(client)) => lines.extend(
                [
                    t!("host.detail.latency", ms = format!("{total:.0}")),
                    t!("host.detail.latency_host", ms = format!("{:.1}", self.host_ms)),
                    t!("host.detail.latency_network", ms = format!("{:.1}", self.rtt_ms / 2.0)),
                    t!("host.detail.latency_screen", ms = format!("{:.1}", client.display_ms)),
                    t!("host.detail.showing", fps = format!("{:.0}", client.shown_fps)),
                    match client.late_frames {
                        0 => t!("host.detail.no_delays"),
                        n => t!("host.detail.delays", count = n, ms = format!("{:.0}", client.worst_late_ms)),
                    },
                ]
                .map(|line| Some(line.into_owned())),
            ),
            _ => lines.push(Some(t!("host.detail.waiting").into_owned())),
        }
        lines
    }
}

struct Entry {
    info: SessionInfo,
    conn: quinn::Connection,
}

#[derive(Default)]
pub struct Registry {
    next_id: AtomicU64,
    sessions: Mutex<BTreeMap<u64, Entry>>,
    /// Held while any screen is connected (see `Activity`).
    activity: Mutex<Option<Activity>>,
}

impl Registry {
    pub fn set_kvm(&self, id: u64, role: KvmRole) {
        if let Some(entry) = self.sessions.lock().unwrap().get_mut(&id) {
            entry.info.kvm = Some(role);
        }
    }

    /// The session's client id, for settings saved per computer.
    pub fn client_id(&self, id: u64) -> Option<String> {
        self.sessions.lock().unwrap().get(&id).map(|e| e.info.client_id.clone())
    }

    /// Adds a session; it's removed again when the returned guard drops.
    pub fn add(self: &Arc<Self>, mut info: SessionInfo, conn: quinn::Connection) -> SessionGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        info.id = id;
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(id, Entry { info, conn });
        if sessions.len() == 1 {
            *self.activity.lock().unwrap() = Some(Activity::begin("streaming to Freemonplay screens"));
        }
        SessionGuard { registry: self.clone(), id }
    }

    pub fn snapshot(&self) -> Vec<SessionInfo> {
        self.sessions.lock().unwrap().values().map(|e| e.info.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// Ends a session; the client is told why.
    pub fn disconnect(&self, id: u64) {
        if let Some(entry) = self.sessions.lock().unwrap().get(&id) {
            entry.conn.close(0u32.into(), b"disconnected from the host");
        }
    }
}

pub struct SessionGuard {
    registry: Arc<Registry>,
    id: u64,
}

/// How a session's keyboard and mouse monitors connect to this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmRole {
    /// Around the screen it streams.
    Mixed,
    /// Keyboard and mouse only, on this side of the main display.
    Beside(Edge),
}

impl SessionGuard {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn set_kvm(&self, role: KvmRole) {
        self.update(|info| info.kvm = Some(role));
    }

    pub fn update_stats(&self, fps: f32, mbps: f32, host_ms: f32, rtt_ms: f32) {
        self.update(|info| {
            info.fps = fps;
            info.mbps = mbps;
            info.host_ms = host_ms;
            info.rtt_ms = rtt_ms;
        });
    }

    pub fn update_rate(&self, target_bps: u32, max_bps: u32, loss: f64) {
        self.update(|info| {
            info.target_mbps = target_bps as f32 / 1e6;
            info.max_mbps = max_bps as f32 / 1e6;
            info.loss_pct = (loss * 100.0) as f32;
        });
    }

    pub fn update_network(&self, network: NetworkInfo) {
        self.update(|info| info.network = Some(network));
    }

    pub fn update_client(&self, stats: ClientStats) {
        self.update(|info| info.client = Some(stats));
    }

    fn update(&self, change: impl FnOnce(&mut SessionInfo)) {
        if let Some(entry) = self.registry.sessions.lock().unwrap().get_mut(&self.id) {
            change(&mut entry.info);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut sessions = self.registry.sessions.lock().unwrap();
        sessions.remove(&self.id);
        if sessions.is_empty() {
            self.registry.activity.lock().unwrap().take();
        }
    }
}

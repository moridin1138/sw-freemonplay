//! Live list of connected screens, for the menu bar app (and anything else that wants to
//! show or control sessions).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fmp_proto::{ClientStats, Codec, Edge, NetworkInfo};

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

    /// The session's details for its submenu; `None` is a separator. `this` names the host
    /// computer ("This Mac").
    pub fn details(&self, this: &str) -> Vec<Option<String>> {
        let mut lines = vec![
            Some(format!("Address: {}", self.addr.ip().to_canonical())),
            Some(format!("Encoder: {}", self.encoder)),
            Some(if self.target_mbps < self.max_mbps - 0.05 {
                format!("Bitrate: {:.1} of {:.0} Mbit/s (lowered: the network is behind)", self.target_mbps, self.max_mbps)
            } else {
                format!("Bitrate: up to {:.0} Mbit/s", self.max_mbps)
            }),
        ];
        if self.loss_pct >= 0.1 {
            lines.push(Some(format!("Packets lost: {:.1}%", self.loss_pct)));
        }
        if let Some(network) = &self.network {
            lines.push(Some(format!("Screen's network: {}", network.summary)));
            lines.extend(network.advice.iter().map(|tip| Some(format!("    Tip: {tip}"))));
        }
        lines.push(None);
        match (self.latency_ms(), self.client) {
            (Some(total), Some(client)) => lines.extend(
                [
                    format!("Latency: about {total:.0} ms"),
                    format!("    {this} (capture to sent): {:.1} ms", self.host_ms),
                    format!("    Network (half the {:.1} ms round trip): {:.1} ms", self.rtt_ms, self.rtt_ms / 2.0),
                    format!("    Screen (received to shown): {:.1} ms", client.display_ms),
                    format!("Screen is showing {:.0} fps", client.shown_fps),
                    match client.late_frames {
                        0 => "No frames delayed in transit".to_string(),
                        n => format!("{n} frames delayed in transit (worst {:.0} ms)", client.worst_late_ms),
                    },
                    "Plus up to one refresh while the screen waits to display.".to_string(),
                ]
                .map(Some),
            ),
            _ => lines.push(Some("Latency: waiting for the screen's first report".to_string())),
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

//! Live list of connected screens, for the menu bar app (and anything else that wants to
//! show or control sessions).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fmp_proto::{ClientStats, Codec, NetworkInfo};

use crate::platform::Activity;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: u64,
    pub client_name: String,
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
    /// Adds a session; it's removed again when the returned guard drops.
    pub fn add(self: &Arc<Self>, mut info: SessionInfo, conn: quinn::Connection) -> SessionGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        info.id = id;
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(id, Entry { info, conn });
        if sessions.len() == 1 {
            *self.activity.lock().unwrap() = Some(Activity::begin("streaming to freemonplay screens"));
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

impl SessionGuard {
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

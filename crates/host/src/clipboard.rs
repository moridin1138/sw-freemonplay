//! Clipboard sharing: text copied here appears on connected screens' computers, and text
//! copied there appears here (and on the other screens).
//!
//! This side polls the clipboard's change counter about once a second; clients poll theirs
//! and send what changed. Each side remembers what it last sent or received, so text
//! never bounces back. Items marked concealed or transient (what password managers do) are
//! never shared.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use fmp_proto::HostMsg;
use tokio::sync::mpsc::UnboundedSender;

use crate::platform::clipboard as platform;

const POLL: Duration = Duration::from_secs(1);

pub struct Hub {
    state: Mutex<State>,
}

struct State {
    enabled: bool,
    peers: HashMap<u64, Peer>,
    /// The clipboard's change counter when we last looked.
    seen: i64,
}

struct Peer {
    out: UnboundedSender<HostMsg>,
    /// What this peer's clipboard holds, as far as we know.
    has: Option<String>,
}

impl Hub {
    pub fn new(enabled: bool) -> Arc<Self> {
        let hub = Arc::new(Self { state: Mutex::new(State { enabled, peers: HashMap::new(), seen: platform::change_count() }) });
        let weak = Arc::downgrade(&hub);
        let started = std::thread::Builder::new().name("clipboard".into()).spawn(move || poll(weak));
        if let Err(e) = started {
            tracing::warn!("clipboard sharing unavailable: {e}");
        }
        hub
    }

    pub fn set_enabled(&self, enabled: bool) {
        let mut state = self.state.lock().unwrap();
        state.enabled = enabled;
        state.seen = platform::change_count();
    }

    /// A session whose client asked to share the clipboard.
    pub fn register(&self, session: u64, out: UnboundedSender<HostMsg>) {
        self.state.lock().unwrap().peers.insert(session, Peer { out, has: None });
        self.sync_to(session);
    }

    pub fn unregister(&self, session: u64) {
        self.state.lock().unwrap().peers.remove(&session);
    }

    /// Sends the current text to one peer if it doesn't have it yet (on connecting, and
    /// when the keyboard and mouse move over, so a paste right away works).
    pub fn sync_to(&self, session: u64) {
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return;
        }
        let Some(text) = platform::text() else { return };
        if let Some(peer) = state.peers.get_mut(&session) {
            send(peer, &text);
        }
    }

    /// Text a client copied: put it on this clipboard and pass it to the other screens.
    pub fn copied_on_client(&self, session: u64, text: String) {
        let mut state = self.state.lock().unwrap();
        if !state.enabled || text.is_empty() {
            return;
        }
        state.seen = platform::set_text(&text);
        for (&id, peer) in &mut state.peers {
            if id == session {
                peer.has = Some(text.clone());
            } else {
                send(peer, &text);
            }
        }
    }

    fn check(&self) {
        let mut state = self.state.lock().unwrap();
        let count = platform::change_count();
        if !state.enabled || count == state.seen {
            return;
        }
        state.seen = count;
        if let Some(text) = platform::text() {
            for peer in state.peers.values_mut() {
                send(peer, &text);
            }
        }
    }
}

fn send(peer: &mut Peer, text: &str) {
    if peer.has.as_deref() != Some(text) {
        peer.has = Some(text.to_string());
        let _ = peer.out.send(HostMsg::Clipboard(text.to_string()));
    }
}

fn poll(hub: Weak<Hub>) {
    loop {
        std::thread::sleep(POLL);
        let Some(hub) = hub.upgrade() else { return };
        hub.check();
    }
}

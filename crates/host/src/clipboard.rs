//! Clipboard sharing: text and images copied here appear on connected screens' computers,
//! and what's copied there appears here (and on the other screens).
//!
//! This side polls the clipboard's change counter about once a second; clients poll theirs
//! and send what changed. Each side remembers what it last sent or received, so nothing
//! bounces back. Items marked concealed or transient (what password managers do) are never
//! shared. Images travel as PNG on streams of their own (see the session).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use fmp_proto::{HostMsg, MAX_CLIPBOARD_IMAGE_BYTES};
use tokio::sync::mpsc::UnboundedSender;

use crate::platform::clipboard as platform;

const POLL: Duration = Duration::from_secs(1);

/// What's on a clipboard, as far as sharing goes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Text(String),
    /// PNG.
    Image(Vec<u8>),
}

impl Item {
    /// Identifies the content without keeping a copy of every image around.
    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    fn message(&self) -> HostMsg {
        match self {
            Item::Text(text) => HostMsg::Clipboard(text.clone()),
            Item::Image(png) => HostMsg::ClipboardImage(png.clone()),
        }
    }
}

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
    has: Option<u64>,
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

    /// Sends the current item to one peer if it doesn't have it yet (on connecting, and
    /// when the keyboard and mouse move over, so a paste right away works).
    pub fn sync_to(&self, session: u64) {
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return;
        }
        let Some(item) = read() else { return };
        if let Some(peer) = state.peers.get_mut(&session) {
            send(peer, &item);
        }
    }

    /// Something a client copied: put it on this clipboard and pass it to the other screens.
    pub fn copied_on_client(&self, session: u64, item: Item) {
        let mut state = self.state.lock().unwrap();
        if !state.enabled || matches!(&item, Item::Text(t) if t.is_empty()) {
            return;
        }
        state.seen = platform::write(&item);
        let fingerprint = item.fingerprint();
        for (&id, peer) in &mut state.peers {
            if id == session {
                peer.has = Some(fingerprint);
            } else {
                send(peer, &item);
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
        if let Some(item) = read() {
            for peer in state.peers.values_mut() {
                send(peer, &item);
            }
        }
    }
}

/// This computer's clipboard, unless it's private, empty or an oversized image.
fn read() -> Option<Item> {
    let item = platform::read()?;
    if let Item::Image(png) = &item
        && png.len() > MAX_CLIPBOARD_IMAGE_BYTES
    {
        tracing::info!("not sharing a {} MB copied image (too big)", png.len() / 1_000_000);
        return None;
    }
    Some(item)
}

fn send(peer: &mut Peer, item: &Item) {
    let fingerprint = item.fingerprint();
    if peer.has != Some(fingerprint) {
        peer.has = Some(fingerprint);
        let _ = peer.out.send(item.message());
    }
}

fn poll(hub: Weak<Hub>) {
    loop {
        std::thread::sleep(POLL);
        let Some(hub) = hub.upgrade() else { return };
        hub.check();
    }
}

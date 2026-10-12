//! Virtual displays kept for a little while after their screen's connection was lost (not
//! closed on purpose): Wi-Fi dropping out, or a laptop roaming between access points. If
//! the screen reconnects in time it gets the same display back, with its windows still on
//! it; otherwise the display goes, as it would have straight away.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fmp_proto::DisplayRequest;

use crate::platform::VirtualDisplay;

/// How long a lost screen's display waits for it.
pub const GRACE: Duration = Duration::from_secs(15);

struct Parked {
    display: Arc<VirtualDisplay>,
    req: DisplayRequest,
    /// Tells this parking apart from a later one under the same key.
    generation: u64,
}

#[derive(Default)]
pub struct Parking {
    slots: Mutex<HashMap<String, Parked>>,
    generation: Mutex<u64>,
}

impl Parking {
    /// Keeps `display` for `key` (the screen's identity) for `GRACE`, then drops it.
    pub fn park(self: &Arc<Self>, key: String, display: Arc<VirtualDisplay>, req: DisplayRequest) {
        let generation = {
            let mut g = self.generation.lock().unwrap();
            *g += 1;
            *g
        };
        let replaced = self.slots.lock().unwrap().insert(key.clone(), Parked { display, req, generation });
        drop_display(replaced.map(|p| p.display));
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(GRACE).await;
            let expired = {
                let mut slots = this.slots.lock().unwrap();
                match slots.get(&key) {
                    Some(p) if p.generation == generation => slots.remove(&key),
                    _ => None,
                }
            };
            if let Some(parked) = expired {
                tracing::info!("{key}: didn't reconnect within {} s; removing its display", GRACE.as_secs());
                drop_display(Some(parked.display));
            }
        });
    }

    /// The display kept for `key`, if the screen wants the same kind of display again.
    /// A different request drops the kept one, so a new one can take its place. Call it
    /// off the async threads (unplugging a display can block).
    pub fn take(&self, key: &str, req: &DisplayRequest) -> Option<Arc<VirtualDisplay>> {
        let parked = self.slots.lock().unwrap().remove(key)?;
        let same = (parked.req.width, parked.req.height, parked.req.hidpi, parked.req.scale)
            == (req.width, req.height, req.hidpi, req.scale);
        if same {
            return Some(parked.display);
        }
        // Gone before a new display (with the same serial number) replaces it.
        drop(parked);
        None
    }

    /// Drops every kept display now (quitting).
    pub fn clear(&self) {
        let parked: Vec<Parked> = self.slots.lock().unwrap().drain().map(|(_, p)| p).collect();
        drop(parked);
    }
}

/// Unplugging a virtual display can block briefly; keep it off the async threads.
fn drop_display(display: Option<Arc<VirtualDisplay>>) {
    if let Some(display) = display {
        tokio::task::spawn_blocking(move || drop(display));
    }
}

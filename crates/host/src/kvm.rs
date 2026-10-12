//! Keyboard and mouse mode (a software KVM): this computer's keyboard and mouse control a
//! client's own monitors, without streaming them.
//!
//! Sessions that offer such monitors register a [`Link`]. The platform layer watches the
//! pointer (macOS: an event tap) and asks [`Kvm::check_entry`] whether it's pushing off an
//! edge that leads to one. If so it takes over: the local cursor is frozen and hidden,
//! and all mouse and keyboard input goes to that client ([`Kvm::forward`]) until the
//! client hands it back ([`Kvm::return_control`]) or the escape shortcut is pressed.
//!
//! Two layouts:
//! - **Mixed:** the session also streams a screen (a virtual display here). The client's
//!   keyboard and mouse monitors sit around the monitor showing it, as on the client's
//!   desk, so leaving that virtual display toward them crosses over.
//! - **Beside:** keyboard and mouse only. The client's monitors sit on one side of this
//!   computer's main display (chosen in the menu).

use std::sync::{Arc, Mutex};

use fmp_proto::{Edge, HostMsg, KvmLayout, Rect, RemoteInput, ReturnAt};
use tokio::sync::mpsc::UnboundedSender;

use crate::clipboard::Hub;
use crate::platform::kvm as platform;

/// A display's area in this computer's global coordinates (macOS: points, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where a session's keyboard and mouse monitors connect to this computer's displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Mixed layout: around the session's own virtual display.
    Screen { display: u32 },
    /// Keyboard and mouse only: on `side` of `display`.
    Beside { display: u32, side: Edge },
}

struct Link {
    session: u64,
    name: String,
    out: UnboundedSender<HostMsg>,
    layout: KvmLayout,
    anchor: Anchor,
}

struct Active {
    session: u64,
    /// Where the local cursor is parked (frozen) meanwhile.
    park: (f64, f64),
}

#[derive(Default)]
struct State {
    links: Vec<Link>,
    active: Option<Active>,
}

pub struct Kvm {
    state: Mutex<State>,
    /// So a paste works the moment the keyboard and mouse arrive.
    clipboard: Arc<Hub>,
}

/// The pointer, as the platform layer saw it move.
pub struct Pointer {
    pub display: u32,
    pub bounds: Bounds,
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
}

impl Kvm {
    pub fn new(clipboard: Arc<Hub>) -> Arc<Self> {
        Arc::new(Self { state: Mutex::default(), clipboard })
    }

    /// Offers a session's keyboard and mouse monitors. Starts watching the pointer if this is
    /// the first.
    pub fn register(
        self: &Arc<Self>,
        session: u64,
        name: &str,
        out: UnboundedSender<HostMsg>,
        layout: KvmLayout,
        anchor: Anchor,
    ) {
        tracing::info!("{name}: keyboard and mouse can move onto {} of its monitors", layout.monitors.len());
        self.state.lock().unwrap().links.push(Link { session, name: name.to_string(), out, layout, anchor });
        if let Err(e) = platform::start(self.clone()) {
            tracing::warn!("keyboard and mouse mode unavailable: {e:#}");
        }
    }

    pub fn unregister(&self, session: u64) {
        let mut state = self.state.lock().unwrap();
        state.links.retain(|l| l.session != session);
        if state.active.as_ref().is_some_and(|a| a.session == session) {
            let active = state.active.take().unwrap();
            drop(state);
            tracing::info!("keyboard and mouse back (that screen disconnected)");
            platform::release_cursor(active.park);
        }
    }

    /// Changes where a keyboard-and-mouse-only session's monitors sit (from the menu).
    pub fn set_side(&self, session: u64, side: Edge) {
        let mut state = self.state.lock().unwrap();
        if let Some(link) = state.links.iter_mut().find(|l| l.session == session)
            && let Anchor::Beside { display, .. } = link.anchor
        {
            link.anchor = Anchor::Beside { display, side };
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().active.is_some()
    }

    /// Pointer motion while this computer has its keyboard and mouse. Returns true when it
    /// crossed onto a client, which then has them (the cursor is already parked).
    pub fn check_entry(&self, p: &Pointer) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.active.is_some() {
            return false;
        }
        let Some((edge, along)) = pushing_edge(&p.bounds, p.x, p.y, p.dx, p.dy) else { return false };
        let found = state.links.iter().enumerate().find_map(|(i, link)| {
            let entry = match link.anchor {
                Anchor::Screen { display } if display == p.display => {
                    let screen = link.layout.screen?;
                    entry_from_screen(&screen, &link.layout.monitors, edge, along)
                }
                Anchor::Beside { display, side } if display == p.display && side == edge => {
                    entry_beside(&link.layout.monitors, side, along)
                }
                _ => None,
            }?;
            Some((i, entry))
        });
        let Some((i, (monitor, x, y, back))) = found else { return false };

        let link = &state.links[i];
        self.clipboard.sync_to(link.session);
        if link.out.send(HostMsg::RemoteEnter { monitor: monitor as u32, x, y, back }).is_err() {
            return false;
        }
        tracing::info!("keyboard and mouse now control {}", link.name);
        let (session, park) = (link.session, park_point(&p.bounds, p.x, p.y));
        state.active = Some(Active { session, park });
        drop(state);
        platform::capture_cursor(park);
        true
    }

    /// Input while a client has the keyboard and mouse. False if it's gone (control is
    /// then back here).
    pub fn forward(&self, input: RemoteInput) -> bool {
        let state = self.state.lock().unwrap();
        let Some(active) = &state.active else { return false };
        let sent = state.links.iter().find(|l| l.session == active.session).is_some_and(|l| l.out.send(HostMsg::Remote(input)).is_ok());
        drop(state);
        if !sent {
            self.release("the screen stopped responding");
        }
        sent
    }

    /// Takes the keyboard and mouse back here (escape shortcut, or trouble), putting the
    /// cursor back where it left.
    pub fn release(&self, why: &str) {
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.active.take() else { return };
        if let Some(link) = state.links.iter().find(|l| l.session == active.session) {
            let _ = link.out.send(HostMsg::RemoteLeave);
        }
        drop(state);
        tracing::info!("keyboard and mouse back ({why})");
        platform::release_cursor(active.park);
    }

    /// The client moved the pointer back toward this computer.
    pub fn return_control(&self, session: u64, at: ReturnAt) {
        let mut state = self.state.lock().unwrap();
        if !state.active.as_ref().is_some_and(|a| a.session == session) {
            return;
        }
        let anchor = state.links.iter().find(|l| l.session == session).map(|l| l.anchor);
        let park = state.active.take().unwrap().park;
        drop(state);
        let to = anchor.and_then(|anchor| {
            let display = match anchor {
                Anchor::Screen { display } | Anchor::Beside { display, .. } => display,
            };
            return_point(&platform::display_bounds(display)?, anchor, at)
        });
        tracing::info!("keyboard and mouse back");
        platform::release_cursor(to.unwrap_or(park));
    }
}

/// Whether the pointer is pushing against an edge of its display (macOS keeps it at the
/// edge when nothing lies beyond), and how far along that edge (0 to 1).
pub fn pushing_edge(b: &Bounds, x: f64, y: f64, dx: f64, dy: f64) -> Option<(Edge, f64)> {
    let along_y = ((y - b.y) / b.height).clamp(0.0, 1.0);
    let along_x = ((x - b.x) / b.width).clamp(0.0, 1.0);
    if dx > 0.0 && x >= b.x + b.width - 1.0 {
        Some((Edge::Right, along_y))
    } else if dx < 0.0 && x <= b.x {
        Some((Edge::Left, along_y))
    } else if dy > 0.0 && y >= b.y + b.height - 1.0 {
        Some((Edge::Bottom, along_x))
    } else if dy < 0.0 && y <= b.y {
        Some((Edge::Top, along_x))
    } else {
        None
    }
}

/// Mixed layout: leaving the streamed screen at `edge`, `along` it. Returns the client
/// monitor beyond that point and where on it (0 to 1).
fn entry_from_screen(screen: &Rect, monitors: &[Rect], edge: Edge, along: f64) -> Option<(usize, f32, f32, Option<Edge>)> {
    let (sx, sy, sw, sh) = (screen.x as f64, screen.y as f64, screen.width as f64, screen.height as f64);
    let (px, py) = match edge {
        Edge::Right => (sx + sw + 0.5, sy + along * sh),
        Edge::Left => (sx - 0.5, sy + along * sh),
        Edge::Bottom => (sx + along * sw, sy + sh + 0.5),
        Edge::Top => (sx + along * sw, sy - 0.5),
    };
    let i = monitors.iter().position(|m| m.contains(px, py))?;
    let (x, y) = normalized(&monitors[i], px, py);
    Some((i, x, y, None))
}

/// Keyboard and mouse only: leaving this computer's display on `side`, `along` it. The
/// client's monitors (as a block) sit beyond it; enter at the matching point of their
/// facing edge.
fn entry_beside(monitors: &[Rect], side: Edge, along: f64) -> Option<(usize, f32, f32, Option<Edge>)> {
    let left = monitors.iter().map(|m| m.x).min()? as f64;
    let top = monitors.iter().map(|m| m.y).min()? as f64;
    let right = monitors.iter().map(Rect::right).max()? as f64;
    let bottom = monitors.iter().map(Rect::bottom).max()? as f64;
    let (px, py) = match side {
        Edge::Right => (left + 0.5, top + along * (bottom - top)),
        Edge::Left => (right - 0.5, top + along * (bottom - top)),
        Edge::Bottom => (left + along * (right - left), top + 0.5),
        Edge::Top => (left + along * (right - left), bottom - 0.5),
    };
    // The nearest monitor, in case the point falls in a gap between them.
    let i = (0..monitors.len()).min_by(|&a, &b| distance(&monitors[a], px, py).total_cmp(&distance(&monitors[b], px, py)))?;
    let m = &monitors[i];
    let px = px.clamp(m.x as f64, m.right() as f64 - 1.0);
    let py = py.clamp(m.y as f64, m.bottom() as f64 - 1.0);
    let (x, y) = normalized(m, px, py);
    Some((i, x, y, Some(side.opposite())))
}

/// Where the cursor reappears here when the client hands control back.
fn return_point(b: &Bounds, anchor: Anchor, at: ReturnAt) -> Option<(f64, f64)> {
    let inside = |x: f64, y: f64| {
        (x.clamp(b.x + 2.0, b.x + b.width - 3.0), y.clamp(b.y + 2.0, b.y + b.height - 3.0))
    };
    match (anchor, at) {
        (Anchor::Screen { .. }, ReturnAt::Screen { x, y }) => {
            Some(inside(b.x + x as f64 * b.width, b.y + y as f64 * b.height))
        }
        (Anchor::Beside { side, .. }, ReturnAt::Edge { along }) => {
            let along = along.clamp(0.0, 1.0) as f64;
            Some(match side {
                Edge::Right => inside(b.x + b.width, b.y + along * b.height),
                Edge::Left => inside(b.x, b.y + along * b.height),
                Edge::Bottom => inside(b.x + along * b.width, b.y + b.height),
                Edge::Top => inside(b.x + along * b.width, b.y),
            })
        }
        _ => None,
    }
}

/// Just inside the edge the pointer left by, so it doesn't immediately cross again.
fn park_point(b: &Bounds, x: f64, y: f64) -> (f64, f64) {
    (x.clamp(b.x + 2.0, b.x + b.width - 3.0), y.clamp(b.y + 2.0, b.y + b.height - 3.0))
}

fn normalized(m: &Rect, x: f64, y: f64) -> (f32, f32) {
    (((x - m.x as f64) / m.width as f64) as f32, ((y - m.y as f64) / m.height as f64) as f32)
}

fn distance(m: &Rect, x: f64, y: f64) -> f64 {
    let dx = (m.x as f64 - x).max(x - m.right() as f64).max(0.0);
    let dy = (m.y as f64 - y).max(y - m.bottom() as f64).max(0.0);
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Bounds = Bounds { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 };

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn detects_pushing_against_an_edge() {
        assert_eq!(pushing_edge(&DISPLAY, 1439.0, 450.0, 3.0, 0.0), Some((Edge::Right, 0.5)));
        assert_eq!(pushing_edge(&DISPLAY, 1439.0, 450.0, -3.0, 0.0), None);
        assert_eq!(pushing_edge(&DISPLAY, 700.0, 0.0, 0.0, -2.0), Some((Edge::Top, 700.0 / 1440.0)));
        assert_eq!(pushing_edge(&DISPLAY, 700.0, 450.0, 5.0, 5.0), None);
    }

    #[test]
    fn mixed_layout_enters_the_monitor_beyond_the_screen() {
        // Streamed screen in the middle, keyboard and mouse monitors either side.
        let screen = rect(0, 0, 2736, 1824);
        let monitors = [rect(-1920, 0, 1920, 1080), rect(2736, 200, 2560, 1440)];
        let (i, x, y, back) = entry_from_screen(&screen, &monitors, Edge::Right, 0.5).unwrap();
        assert_eq!((i, back), (1, None));
        assert!(x < 0.01 && (y - (912.0 - 200.0) / 1440.0).abs() < 0.001);
        assert_eq!(entry_from_screen(&screen, &monitors, Edge::Left, 0.25).unwrap().0, 0);
        // Nothing beyond the lower part of the left edge (that monitor is shorter).
        assert_eq!(entry_from_screen(&screen, &monitors, Edge::Left, 0.9), None);
        assert_eq!(entry_from_screen(&screen, &monitors, Edge::Top, 0.5), None);
    }

    #[test]
    fn beside_layout_enters_the_facing_edge() {
        let monitors = [rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080)];
        let (i, x, y, back) = entry_beside(&monitors, Edge::Right, 0.5).unwrap();
        assert_eq!((i, back), (0, Some(Edge::Left)));
        assert!(x < 0.01 && (y - 0.5).abs() < 0.01);
        let (i, x, _, back) = entry_beside(&monitors, Edge::Left, 0.5).unwrap();
        assert_eq!((i, back), (1, Some(Edge::Right)));
        assert!(x > 0.99);
    }

    #[test]
    fn returns_to_the_matching_point() {
        let screen = Anchor::Screen { display: 1 };
        assert_eq!(return_point(&DISPLAY, screen, ReturnAt::Screen { x: 0.5, y: 0.5 }), Some((720.0, 450.0)));
        assert_eq!(return_point(&DISPLAY, screen, ReturnAt::Screen { x: 1.0, y: 0.0 }), Some((1437.0, 2.0)));
        let beside = Anchor::Beside { display: 1, side: Edge::Right };
        assert_eq!(return_point(&DISPLAY, beside, ReturnAt::Edge { along: 0.5 }), Some((1437.0, 450.0)));
        assert_eq!(return_point(&DISPLAY, beside, ReturnAt::Screen { x: 0.5, y: 0.5 }), None);
    }
}

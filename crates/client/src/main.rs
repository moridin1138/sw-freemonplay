//! freemonplay client: turns this screen into an extra monitor for a freemonplay host.
//!
//! Shortcuts (not forwarded to the host):
//!   Ctrl+Alt+Shift+Q  quit
//!   Ctrl+Alt+Shift+F  toggle full screen

mod decode;
mod input;
mod net;
mod render;
mod store;

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use fmp_proto::{Auth, ClientMsg, ClientStats, Codec, DisplayRequest, InputEvent, TouchPhase};
use tokio::sync::{mpsc, oneshot};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use decode::FrameSlot;
use render::Renderer;
use store::ClientStore;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Host to connect to: name, IP, or IP:port. Without it, hosts on the LAN are discovered.
    #[arg(long)]
    host: Option<String>,
    /// Pairing code shown by the host. Only needed the first time; asked for if missing.
    #[arg(long)]
    code: Option<String>,
    /// Which monitor to fill (0 = first). Defaults to the primary monitor.
    #[arg(long)]
    monitor: Option<usize>,
    /// Run in a window of this size (e.g. 1280x800) instead of full screen.
    #[arg(long, value_name = "WxH")]
    windowed: Option<String>,
    /// Retina-style 2x rendering on the host. Auto uses it on high-density screens.
    #[arg(long, value_enum, default_value_t = Scale::Auto)]
    scale: Scale,
    /// Video codec to ask for. Auto offers HEVC first when this machine decodes it in
    /// hardware, with H.264 as the fallback.
    #[arg(long, value_enum, default_value_t = CodecArg::Auto)]
    codec: CodecArg,
    /// Show frames as soon as they're decoded instead of at the display's next refresh.
    /// Saves up to one refresh of latency (about 16 ms at 60 Hz) but the picture can tear.
    #[arg(long)]
    no_vsync: bool,
    /// Name this screen shows up as on the host. Defaults to this computer's hostname.
    #[arg(long)]
    name: Option<String>,
    /// Where to keep this client's id and paired hosts.
    #[arg(long)]
    config_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Scale {
    Auto,
    #[value(name = "1")]
    One,
    #[value(name = "2")]
    Two,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecArg {
    Auto,
    H264,
    H265,
}

#[derive(Debug)]
pub enum UserEvent {
    Connected { host_name: String, width: u32, height: u32, codec: Codec },
    Frame,
    Closed(String),
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu=warn,wgpu_core=error,wgpu_hal=error,naga=warn,quinn=warn,mdns_sd=warn".into()),
        )
        .init();
    let args = Args::parse();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let windowed = args.windowed.as_deref().map(parse_size).transpose()?;

    let rt = tokio::runtime::Runtime::new()?;
    let store = ClientStore::open(args.config_dir.clone())?;
    let target = rt.block_on(choose_target(args.host.as_deref()))?;
    println!("Connecting to {target}");
    let mut connected = rt.block_on(net::connect_any(&target))?;

    let auth = match store.host(&connected.fingerprint).and_then(|h| store::unhex(&h.token_hex)) {
        Some(token) => Auth::Token(token),
        None => {
            let code = match &args.code {
                Some(code) => code.clone(),
                None => {
                    // The host only waits a few seconds for us to start the session, so don't
                    // hold the connection open while a person types: ask, then reconnect.
                    let fingerprint = connected.fingerprint.clone();
                    connected.conn.close(0u32.into(), b"pairing");
                    let code = prompt(&format!(
                        "First time connecting to {}. Enter the pairing code it shows: ",
                        target.name
                    ))?;
                    connected = rt.block_on(net::connect_any(&target))?;
                    if connected.fingerprint != fingerprint {
                        bail!("the host's identity changed while pairing; not sending the code");
                    }
                    code
                }
            };
            Auth::PairingCode(code)
        }
    };

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let (display_tx, display_rx) = oneshot::channel();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let frames = Arc::new(FrameSlot::default());

    let session = net::Session {
        connected,
        auth,
        client_name: args.name.clone().unwrap_or_else(hostname),
        codecs: match args.codec {
            CodecArg::Auto => decode::supported_codecs(),
            CodecArg::H264 => vec![Codec::H264],
            CodecArg::H265 => vec![Codec::H265],
        },
        store,
        display: display_rx,
        outgoing: out_rx,
        outgoing_tx: out_tx.clone(),
        frames: frames.clone(),
        proxy: proxy.clone(),
    };
    rt.spawn(async move {
        let reason = net::run(session).await.unwrap_or_else(|e| format!("{e:#}"));
        let _ = proxy.send_event(UserEvent::Closed(reason));
    });

    let mut app = App {
        monitor: args.monitor,
        windowed,
        scale: args.scale,
        vsync: !args.no_vsync,
        host_label: target.name,
        display_tx: Some(display_tx),
        out: out_tx,
        frames,
        window: None,
        renderer: None,
        video: None,
        modifiers: ModifiersState::empty(),
        held_keys: HashSet::new(),
        exit_reason: None,
        shown: ShownStats::new(),
    };
    event_loop.run_app(&mut app)?;
    rt.shutdown_timeout(Duration::from_millis(200));
    match app.exit_reason {
        Some(reason) if reason != "closed" => bail!("disconnected: {reason}"),
        _ => Ok(()),
    }
}

async fn choose_target(host: Option<&str>) -> Result<net::Target> {
    if let Some(spec) = host {
        return net::resolve(spec).await;
    }
    println!("Looking for freemonplay hosts on the network...");
    let mut found = tokio::task::spawn_blocking(|| net::discover(Duration::from_secs(3))).await??;
    match found.len() {
        0 => bail!(
            "no hosts found. Is the host running on the same network? Firewalls can block discovery; \
             you can also pass --host <ip>"
        ),
        1 => Ok(found.remove(0)),
        _ => {
            for (i, t) in found.iter().enumerate() {
                println!("  {}) {t}", i + 1);
            }
            let pick: usize = prompt("Which host? ")?.parse().context("enter a number")?;
            if pick == 0 || pick > found.len() {
                bail!("no host number {pick}");
            }
            Ok(found.remove(pick - 1))
        }
    }
}

struct App {
    monitor: Option<usize>,
    windowed: Option<(u32, u32)>,
    scale: Scale,
    vsync: bool,
    host_label: String,
    display_tx: Option<oneshot::Sender<DisplayRequest>>,
    out: mpsc::UnboundedSender<ClientMsg>,
    frames: Arc<FrameSlot>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// Stream resolution, known once the host welcomes us.
    video: Option<(u32, u32)>,
    modifiers: ModifiersState,
    held_keys: HashSet<u16>,
    exit_reason: Option<String>,
    shown: ShownStats,
}

impl App {
    fn send(&self, event: InputEvent) {
        let _ = self.out.send(ClientMsg::Input(event));
    }

    fn normalized(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let video = self.video?;
        let size = self.window.as_ref()?.inner_size();
        Some(input::normalize((x, y), video, (size.width, size.height)))
    }

    /// Uploads the newest decoded picture, if any, and presents.
    fn draw(&mut self) {
        let Some(r) = &mut self.renderer else { return };
        match self.frames.take() {
            Some(frame) => {
                r.upload(&frame);
                r.render();
                self.shown.add(frame.received_at.map(|at| at.elapsed()));
                if let Some(stats) = self.shown.take_report() {
                    let _ = self.out.send(ClientMsg::Stats(stats));
                }
            }
            None => r.render(),
        }
    }

    fn release_all_keys(&mut self) {
        for usage in std::mem::take(&mut self.held_keys) {
            self.send(InputEvent::Key { usage, pressed: false });
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, reason: String) {
        self.exit_reason = Some(reason);
        event_loop.exit();
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        for (i, m) in monitors.iter().enumerate() {
            let s = m.size();
            tracing::debug!("monitor {i}: {:?} {}x{} @{}x", m.name(), s.width, s.height, m.scale_factor());
        }
        let monitor = match self.monitor {
            Some(i) => Some(monitors.get(i).cloned().with_context(|| format!("no monitor {i}"))?),
            None => event_loop.primary_monitor().or_else(|| monitors.first().cloned()),
        };

        let title = format!("freemonplay: {}", self.host_label);
        let mut attrs = Window::default_attributes().with_title(title);
        let (width, height, refresh_hz, density) = match (self.windowed, &monitor) {
            (Some((w, h)), m) => {
                attrs = attrs.with_inner_size(PhysicalSize::new(w, h));
                (w, h, 60, m.as_ref().map_or(1.0, |m| m.scale_factor()))
            }
            (None, Some(m)) => {
                attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
                let refresh = m.refresh_rate_millihertz().map_or(60, |mhz| (mhz + 500) / 1000);
                (m.size().width, m.size().height, refresh, m.scale_factor())
            }
            (None, None) => bail!("no monitor found; try --windowed 1280x800"),
        };
        let hidpi = match self.scale {
            Scale::Auto => density >= 1.5 && self.windowed.is_none(),
            Scale::One => false,
            Scale::Two => true,
        };

        let window = Arc::new(event_loop.create_window(attrs)?);
        // The host draws its cursor into the video; a second local one would just be confusing.
        window.set_cursor_visible(false);
        self.renderer = Some(Renderer::new(window.clone(), self.vsync)?);
        self.window = Some(window);

        let request = DisplayRequest { width, height, refresh_hz, hidpi, size_mm: None };
        tracing::info!("requesting a {width}x{height}@{refresh_hz} display (hidpi {hidpi})");
        if let Some(tx) = self.display_tx.take() {
            let _ = tx.send(request);
        }
        Ok(())
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(e) = self.open_window(event_loop)
        {
            self.fail(event_loop, format!("{e:#}"));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Connected { host_name, width, height, codec } => {
                tracing::info!("connected to {host_name}, receiving {width}x{height} {codec}");
                self.video = Some((width, height));
                if let Some(w) = &self.window {
                    w.set_title(&format!("freemonplay: {host_name}"));
                }
            }
            // Draw right away: request_redraw would wait for the next display refresh tick,
            // adding up to a frame of latency.
            UserEvent::Frame => self.draw(),
            UserEvent::Closed(reason) => self.fail(event_loop, reason),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.fail(event_loop, "closed".into()),
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Focused(false) => self.release_all_keys(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let pressed = event.state == ElementState::Pressed;
                let m = self.modifiers;
                if pressed && m.control_key() && m.alt_key() && m.shift_key() {
                    match code {
                        KeyCode::KeyQ => return self.fail(event_loop, "closed".into()),
                        KeyCode::KeyF => {
                            if let Some(w) = &self.window {
                                let full = w.fullscreen().is_some();
                                w.set_fullscreen((!full).then_some(Fullscreen::Borderless(None)));
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                // The host OS generates its own key repeat while a key is held.
                if event.repeat {
                    return;
                }
                let Some(usage) = input::hid_usage(code) else { return };
                if pressed {
                    self.held_keys.insert(usage);
                } else {
                    self.held_keys.remove(&usage);
                }
                self.send(InputEvent::Key { usage, pressed });
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some((x, y)) = self.normalized(position.x, position.y) {
                    self.send(InputEvent::PointerMove { x, y });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(button) = input::button(button) {
                    self.send(InputEvent::PointerButton { button, pressed: state == ElementState::Pressed });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let event = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => InputEvent::Scroll { dx, dy, precise: false },
                    MouseScrollDelta::PixelDelta(p) => {
                        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                        InputEvent::Scroll { dx: (p.x / scale) as f32, dy: (p.y / scale) as f32, precise: true }
                    }
                };
                self.send(event);
            }
            WindowEvent::Touch(t) => {
                if let Some((x, y)) = self.normalized(t.location.x, t.location.y) {
                    let phase = match t.phase {
                        winit::event::TouchPhase::Started => TouchPhase::Started,
                        winit::event::TouchPhase::Moved => TouchPhase::Moved,
                        winit::event::TouchPhase::Ended => TouchPhase::Ended,
                        winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
                    };
                    self.send(InputEvent::Touch { id: t.id, phase, x, y });
                }
            }
            _ => {}
        }
    }
}

/// Frames shown and their received-to-shown time, reported to the host every 2 s and
/// logged every 10 s.
struct ShownStats {
    since: std::time::Instant,
    frames: u32,
    timed: u32,
    total: Duration,
    reports: u32,
}

impl ShownStats {
    fn new() -> Self {
        Self { since: std::time::Instant::now(), frames: 0, timed: 0, total: Duration::ZERO, reports: 0 }
    }

    fn add(&mut self, received_to_shown: Option<Duration>) {
        self.frames += 1;
        if let Some(d) = received_to_shown {
            self.timed += 1;
            self.total += d;
        }
    }

    fn take_report(&mut self) -> Option<ClientStats> {
        let secs = self.since.elapsed().as_secs_f64();
        if secs < 2.0 || self.timed == 0 {
            return None;
        }
        let stats = ClientStats {
            shown_fps: (self.frames as f64 / secs) as f32,
            display_ms: (self.total.as_secs_f64() * 1e3 / self.timed as f64) as f32,
            rtt_ms: 0.0, // filled in by the network side
        };
        self.reports += 1;
        if self.reports % 5 == 0 {
            tracing::info!("showing {:.0} fps, {:.1} ms from received to shown", stats.shown_fps, stats.display_ms);
        }
        *self = Self { reports: self.reports, ..Self::new() };
        Some(stats)
    }
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').context("expected WxH, e.g. 1280x800")?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

fn prompt(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "freemonplay client".into())
}

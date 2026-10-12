//! The client app: one window that shows the host list (egui) until a session starts, then
//! the host's virtual display, with input forwarded. Disconnecting returns to the list.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmp_proto::{ClientMsg, ClientStats, Codec, DisplayRequest, InputEvent, TouchPhase};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::awake::KeepAwake;
use crate::decode::{self, FrameSlot};
use crate::input;
use crate::net::{self, Credentials, HostList, Prepared, Target};
use crate::render::{Renderer, UiFrame};
use crate::store::ClientStore;
use crate::ui::{self, Action, HostRow, Options, Screen};
use crate::{CodecArg, Scale};

const PICKER_SIZE: LogicalSize<f64> = LogicalSize::new(580.0, 560.0);
const DEFAULT_WINDOWED: (u32, u32) = (1280, 800);

pub enum UserEvent {
    HostsChanged,
    /// A connect attempt finished (successfully or not). `code` is the code it used, if any.
    Prepared { result: Result<(Prepared, Target), String>, code: Option<String> },
    Connected { host_name: String, width: u32, height: u32, codec: Codec },
    Frame,
    Closed(String),
}

/// From the command line; used once at startup.
pub struct Startup {
    pub monitor: Option<usize>,
    pub windowed: Option<(u32, u32)>,
    pub host: Option<String>,
    pub code: Option<String>,
}

/// A session in progress.
struct Live {
    out: mpsc::UnboundedSender<ClientMsg>,
    conn: quinn::Connection,
    /// Set when pairing with a code, to go back to the code dialog if it fails.
    pairing: Option<(Target, String)>,
    connected: bool,
    disconnecting: bool,
}

pub struct App {
    rt: tokio::runtime::Handle,
    proxy: EventLoopProxy<UserEvent>,
    store: Arc<Mutex<ClientStore>>,
    hosts: HostList,
    client_name: String,
    startup: Startup,
    options: Options,
    screen: Screen,
    message: Option<String>,
    address: String,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    repaint_at: Option<Instant>,
    /// The connect attempt or session currently running.
    task: Option<JoinHandle<()>>,
    live: Option<Live>,
    frames: Arc<FrameSlot>,
    /// Stream resolution, known once the host welcomes us.
    video: Option<(u32, u32)>,
    modifiers: ModifiersState,
    held_keys: HashSet<u16>,
    shown: ShownStats,
    /// Show the shortcut hint over the video until then.
    hint_until: Option<Instant>,
    /// Keeps this screen on while connected (when the option is set).
    awake: Option<KeepAwake>,
    pub exit_error: Option<String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rt: tokio::runtime::Handle,
        proxy: EventLoopProxy<UserEvent>,
        store: Arc<Mutex<ClientStore>>,
        hosts: HostList,
        client_name: String,
        startup: Startup,
        options: Options,
    ) -> Self {
        let egui_ctx = egui::Context::default();
        ui::style(&egui_ctx);
        Self {
            rt,
            proxy,
            store,
            hosts,
            client_name,
            startup,
            options,
            screen: Screen::Hosts,
            message: None,
            address: String::new(),
            window: None,
            renderer: None,
            egui_ctx,
            egui_state: None,
            repaint_at: None,
            task: None,
            live: None,
            frames: Arc::new(FrameSlot::default()),
            video: None,
            modifiers: ModifiersState::empty(),
            held_keys: HashSet::new(),
            shown: ShownStats::new(),
            hint_until: None,
            awake: None,
            exit_error: None,
        }
    }

    fn streaming(&self) -> bool {
        matches!(self.screen, Screen::Streaming)
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let mut attrs = Window::default_attributes().with_title("freemonplay").with_inner_size(PICKER_SIZE);
        if let Some(i) = self.startup.monitor {
            let monitor = event_loop.available_monitors().nth(i);
            match monitor {
                Some(m) => {
                    let p = m.position();
                    attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(p.x + 80, p.y + 80));
                }
                None => tracing::warn!("no monitor {i}; using the primary one"),
            }
        }
        let window = Arc::new(event_loop.create_window(attrs)?);
        let renderer = Renderer::new(window.clone(), self.options.vsync)?;
        self.egui_state = Some(egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(renderer.max_texture_side()),
        ));
        self.renderer = Some(renderer);
        self.window = Some(window);
        if let Some(spec) = self.startup.host.take() {
            self.connect_address(spec);
        }
        Ok(())
    }

    // ---- Connecting ------------------------------------------------------------------

    fn connect(&mut self, target: Target, code: Option<String>) {
        self.message = None;
        self.screen = Screen::Connecting { to: target.name.clone() };
        let (store, proxy) = (self.store.clone(), self.proxy.clone());
        self.task = Some(self.rt.spawn(async move {
            let result = net::prepare(&target, &store, code.clone()).await.map(|p| (p, target)).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { result, code });
        }));
        self.redraw();
    }

    fn connect_address(&mut self, spec: String) {
        self.message = None;
        self.screen = Screen::Connecting { to: spec.clone() };
        let (store, proxy) = (self.store.clone(), self.proxy.clone());
        let code = self.startup.code.take();
        self.task = Some(self.rt.spawn(async move {
            let result = async {
                let target = net::resolve(&spec).await?;
                anyhow::Ok((net::prepare(&target, &store, code.clone()).await?, target))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { result, code });
        }));
        self.redraw();
    }

    fn submit_code(&mut self, target: Target, fingerprint: String, code: String) {
        self.screen = Screen::Connecting { to: target.name.clone() };
        let proxy = self.proxy.clone();
        self.task = Some(self.rt.spawn(async move {
            let result = net::prepare_with_code(&target, code.clone(), &fingerprint)
                .await
                .map(|p| (p, target))
                .map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { result, code: Some(code) });
        }));
        self.redraw();
    }

    fn on_prepared(&mut self, result: Result<(Prepared, Target), String>) {
        if !matches!(self.screen, Screen::Connecting { .. }) {
            return; // cancelled meanwhile
        }
        match result {
            Err(e) => {
                self.screen = Screen::Hosts;
                self.message = Some(e);
            }
            Ok((Prepared::NeedCode { fingerprint }, target)) => match self.startup.code.take() {
                Some(code) => self.submit_code(target, fingerprint, code),
                None => self.screen = Screen::Code { target, fingerprint, code: String::new(), error: None },
            },
            Ok((Prepared::Ready(connected, credentials), target)) => self.start_session(connected, credentials, target),
        }
        self.redraw();
    }

    fn start_session(&mut self, connected: net::Connected, credentials: Credentials, target: Target) {
        let pairing = matches!(credentials, Credentials::Code(_)).then(|| (target, connected.fingerprint.clone()));
        let display = self.display_request();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let conn = connected.conn.clone();
        let session = net::Session {
            connected,
            credentials,
            client_name: self.client_name.clone(),
            codecs: match self.options.codec {
                CodecArg::Auto => decode::supported_codecs(),
                CodecArg::H264 => vec![Codec::H264],
                CodecArg::H265 => vec![Codec::H265],
            },
            store: self.store.clone(),
            display,
            gpu_adapter: self.renderer.as_ref().and_then(Renderer::gpu_adapter),
            outgoing: out_rx,
            outgoing_tx: out_tx.clone(),
            frames: self.frames.clone(),
            proxy: self.proxy.clone(),
        };
        let proxy = self.proxy.clone();
        self.task = Some(self.rt.spawn(async move {
            let reason = net::run(session).await.unwrap_or_else(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Closed(reason));
        }));
        self.live = Some(Live { out: out_tx, conn, pairing, connected: false, disconnecting: false });
    }

    /// The virtual display to ask for: the size of the monitor this window is on (or the
    /// windowed size), at its refresh rate.
    fn display_request(&self) -> DisplayRequest {
        let monitor = self.window.as_ref().and_then(|w| w.current_monitor());
        let density = monitor.as_ref().map_or(1.0, |m| m.scale_factor());
        let refresh_hz = monitor.as_ref().and_then(|m| m.refresh_rate_millihertz()).map_or(60, |mhz| (mhz + 500) / 1000);
        let (width, height) = match (&monitor, self.options.fullscreen) {
            (Some(m), true) => (m.size().width, m.size().height),
            _ => self.startup.windowed.unwrap_or(DEFAULT_WINDOWED),
        };
        let hidpi = match self.options.scale {
            Scale::Auto => density >= 1.5 && self.options.fullscreen,
            Scale::One => false,
            Scale::Two => true,
        };
        tracing::info!("requesting a {width}x{height}@{refresh_hz} display (hidpi {hidpi})");
        DisplayRequest { width, height, refresh_hz, hidpi, size_mm: None }
    }

    fn on_connected(&mut self, host_name: String, width: u32, height: u32, codec: Codec) {
        tracing::info!("connected to {host_name}, receiving {width}x{height} {codec}");
        if let Some(live) = &mut self.live {
            live.connected = true;
        }
        self.screen = Screen::Streaming;
        self.video = Some((width, height));
        if self.options.keep_awake {
            self.awake = KeepAwake::start();
        }
        self.hint_until = Some(Instant::now() + Duration::from_secs(4));
        self.repaint_at = self.hint_until;
        if let Some(w) = &self.window {
            w.set_title(&format!("freemonplay: {host_name}"));
            if self.options.fullscreen {
                w.set_fullscreen(Some(Fullscreen::Borderless(w.current_monitor())));
            } else {
                let (ww, wh) = self.startup.windowed.unwrap_or(DEFAULT_WINDOWED);
                let _ = w.request_inner_size(PhysicalSize::new(ww, wh));
            }
            // The host draws its cursor into the video; a second local one would be confusing.
            w.set_cursor_visible(false);
        }
    }

    fn disconnect(&mut self) {
        if let Some(live) = &mut self.live {
            live.disconnecting = true;
            live.conn.close(0u32.into(), b"disconnected by the screen");
        }
    }

    fn on_closed(&mut self, reason: String) {
        let Some(live) = self.live.take() else { return };
        self.task = None;
        self.awake = None;
        self.release_all_keys();
        self.video = None;
        self.hint_until = None;
        if let Some(r) = &mut self.renderer {
            r.clear_video();
        }
        if let Some(w) = &self.window {
            w.set_fullscreen(None);
            w.set_cursor_visible(true);
            w.set_title("freemonplay");
            let _ = w.request_inner_size(PICKER_SIZE);
        }
        // A failed first pairing goes back to the code dialog; anything else to the list.
        match live.pairing {
            Some((target, fingerprint)) if !live.connected && !live.disconnecting => {
                self.screen = Screen::Code {
                    target,
                    fingerprint,
                    code: String::new(),
                    error: Some(reason.trim_start_matches("host refused: ").to_string()),
                };
            }
            _ => {
                self.screen = Screen::Hosts;
                self.message = (!live.disconnecting).then(|| format!("Disconnected: {reason}"));
            }
        }
        self.redraw();
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Connect(i) => {
                let target = self.hosts.lock().unwrap().get(i).cloned();
                if let Some(target) = target {
                    self.connect(target, None);
                }
            }
            Action::ConnectAddress(spec) => self.connect_address(spec),
            Action::SubmitCode => {
                if let Screen::Code { target, fingerprint, code, .. } = &self.screen {
                    let (target, fingerprint, code) = (target.clone(), fingerprint.clone(), code.clone());
                    self.submit_code(target, fingerprint, code);
                }
            }
            Action::CancelCode => self.screen = Screen::Hosts,
            Action::CancelConnect => {
                if let Some(task) = self.task.take() {
                    task.abort();
                }
                self.live = None;
                self.screen = Screen::Hosts;
            }
        }
        self.redraw();
    }

    // ---- Drawing ---------------------------------------------------------------------

    /// The host list, code dialog or connecting view.
    fn draw_ui(&mut self) {
        let (Some(window), Some(state)) = (self.window.clone(), self.egui_state.as_mut()) else { return };
        let raw = state.take_egui_input(&window);
        let hosts: Vec<HostRow> = {
            let store = self.store.lock().unwrap();
            self.hosts
                .lock()
                .unwrap()
                .iter()
                .map(|t| HostRow {
                    name: t.name.clone(),
                    address: t.addrs.first().map(|a| a.ip().to_string()).unwrap_or_default(),
                    paired: t.id.as_deref().is_some_and(|id| store.is_paired(id)),
                })
                .collect()
        };
        let ctx = self.egui_ctx.clone();
        let mut action = None;
        let output = ctx.run_ui(raw, |ui| {
            action = ui::draw(
                ui,
                ui::View {
                    screen: &mut self.screen,
                    hosts: &hosts,
                    message: self.message.as_deref(),
                    address: &mut self.address,
                    options: &mut self.options,
                },
            );
        });
        self.present_ui(&window, output);
        if let Some(action) = action {
            self.handle(action);
        }
    }

    /// The video, with the shortcut hint on top for the first few seconds.
    fn draw_stream(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let new_frame = self.frames.take();
        if let (Some(frame), Some(r)) = (&new_frame, &mut self.renderer) {
            r.upload(frame);
        }
        let hint = self.hint_until.is_some_and(|t| Instant::now() < t);
        if hint && let Some(state) = self.egui_state.as_mut() {
            let raw = state.take_egui_input(&window);
            let ctx = self.egui_ctx.clone();
            let output = ctx.run_ui(raw, |ui| {
                egui::Area::new(egui::Id::new("hint")).anchor(egui::Align2::CENTER_TOP, [0.0, 24.0]).show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label("Ctrl+Alt+Shift+D to disconnect, Ctrl+Alt+Shift+F for full screen");
                    });
                });
            });
            self.present_ui(&window, output);
        } else if let Some(r) = &mut self.renderer {
            r.render(None);
        }
        // Counted after presenting, so it covers decode, upload and present.
        if let Some(frame) = new_frame {
            self.shown.add(frame.received_at);
        }
        if let Some(stats) = self.shown.take_report() {
            if let Some(live) = &self.live {
                let _ = live.out.send(ClientMsg::Stats(stats));
            }
        }
    }

    fn present_ui(&mut self, window: &Window, output: egui::FullOutput) {
        if let Some(state) = self.egui_state.as_mut() {
            state.handle_platform_output(window, output.platform_output);
        }
        let primitives = self.egui_ctx.tessellate(output.shapes, output.pixels_per_point);
        if let Some(r) = &mut self.renderer {
            r.render(Some(UiFrame { primitives, textures: output.textures_delta, pixels_per_point: output.pixels_per_point }));
        }
        let delay = output.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        if delay.is_zero() {
            window.request_redraw();
        } else if delay < Duration::from_secs(3600) {
            self.repaint_at = Some(Instant::now() + delay);
        }
    }

    // ---- Input -----------------------------------------------------------------------

    fn send(&self, event: InputEvent) {
        if let Some(live) = &self.live {
            let _ = live.out.send(ClientMsg::Input(event));
        }
    }

    fn normalized(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let video = self.video?;
        let size = self.window.as_ref()?.inner_size();
        Some(input::normalize((x, y), video, (size.width, size.height)))
    }

    fn release_all_keys(&mut self) {
        for usage in std::mem::take(&mut self.held_keys) {
            self.send(InputEvent::Key { usage, pressed: false });
        }
    }

    /// Mouse, keyboard and touch while streaming: forwarded to the host, apart from our
    /// own shortcuts.
    fn stream_input(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::Focused(false) => self.release_all_keys(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let pressed = event.state == ElementState::Pressed;
                let m = self.modifiers;
                if pressed && m.control_key() && m.alt_key() && m.shift_key() {
                    match code {
                        KeyCode::KeyQ => return event_loop.exit(),
                        KeyCode::KeyD => return self.disconnect(),
                        KeyCode::KeyF => {
                            if let Some(w) = &self.window {
                                let full = w.fullscreen().is_some();
                                w.set_fullscreen((!full).then(|| Fullscreen::Borderless(w.current_monitor())));
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

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(e) = self.open_window(event_loop)
        {
            self.exit_error = Some(format!("{e:#}"));
            event_loop.exit();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::HostsChanged => {
                if matches!(self.screen, Screen::Hosts) {
                    self.redraw();
                }
            }
            UserEvent::Prepared { result, code: _ } => self.on_prepared(result),
            UserEvent::Connected { host_name, width, height, codec } => self.on_connected(host_name, width, height, codec),
            // Draw right away: request_redraw would wait for the next display refresh tick,
            // adding up to a frame of latency.
            UserEvent::Frame => {
                if self.streaming() {
                    self.draw_stream();
                }
            }
            UserEvent::Closed(reason) => self.on_closed(reason),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                self.disconnect();
                return event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::RedrawRequested => {
                if self.streaming() { self.draw_stream() } else { self.draw_ui() }
                return;
            }
            _ => {}
        }
        if self.streaming() {
            self.stream_input(event_loop, event);
        } else if let (Some(window), Some(state)) = (self.window.clone(), self.egui_state.as_mut())
            && state.on_window_event(&window, &event).repaint
        {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.repaint_at {
            Some(at) if Instant::now() >= at => {
                self.repaint_at = None;
                self.redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Frames shown and their received-to-shown time, reported to the host every 2 s and
/// logged every 10 s.
struct ShownStats {
    since: Instant,
    frames: u32,
    timed: u32,
    total: Duration,
    reports: u32,
}

impl ShownStats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, timed: 0, total: Duration::ZERO, reports: 0 }
    }

    fn add(&mut self, received_at: Option<Instant>) {
        self.frames += 1;
        if let Some(at) = received_at {
            self.timed += 1;
            self.total += at.elapsed();
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

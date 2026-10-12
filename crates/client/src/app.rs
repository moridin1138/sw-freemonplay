//! The client app: one window that shows the host list (egui) until a session starts, then
//! the host's virtual display, with input forwarded. Disconnecting returns to the list.
//!
//! With extra monitors selected, each one gets a window and a session of its own once the
//! main window has connected (and paired, if needed): the host sees them as separate
//! screens. They all end together.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmp_proto::{ClientMsg, ClientStats, Codec, DisplayRequest, InputEvent, NetworkInfo, ScreenId, TouchPhase};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

use crate::awake::KeepAwake;
use crate::decode::{self, FrameSlot};
use crate::input;
use crate::link::{self, LinkTuning};
use crate::net::{self, Credentials, HostList, Prepared, Target};
use crate::render::{Renderer, UiFrame};
use crate::store::ClientStore;
use crate::ui::{self, Action, HostRow, MonitorRow, Options, Screen};
use crate::{CodecArg, Scale};

const PICKER_SIZE: LogicalSize<f64> = LogicalSize::new(580.0, 600.0);
const DEFAULT_WINDOWED: (u32, u32) = (1280, 800);
/// The display that holds the host list; it always exists.
const MAIN: usize = 0;

/// Session events carry the display (`view`) they belong to.
// Rare events, so the size of the largest variant doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum UserEvent {
    HostsChanged,
    /// A connect attempt finished (successfully or not). `code` is the code it used, if any.
    Prepared { view: usize, result: Result<(Prepared, Target), String>, code: Option<String> },
    Connected { view: usize, host_name: String, width: u32, height: u32, codec: Codec },
    Frame(usize),
    Closed(usize, String),
}

/// From the command line; used once at startup.
pub struct Startup {
    pub monitor: Option<usize>,
    pub all_monitors: bool,
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

/// A window showing one of the host's screens (the main one also shows the host list).
struct Display {
    window: Arc<Window>,
    renderer: Renderer,
    /// Extra monitors: the monitor it fills, and how it's known to the host. `None` for
    /// the main window, which uses whichever monitor it's on.
    extra: Option<(MonitorHandle, ScreenId)>,
    /// The connect attempt or session currently running.
    task: Option<JoinHandle<()>>,
    live: Option<Live>,
    frames: Arc<FrameSlot>,
    /// Stream resolution, known once the host welcomes us.
    video: Option<(u32, u32)>,
    held_keys: HashSet<u16>,
    shown: ShownStats,
}

impl Display {
    fn new(window: Arc<Window>, renderer: Renderer, extra: Option<(MonitorHandle, ScreenId)>) -> Self {
        Self {
            window,
            renderer,
            extra,
            task: None,
            live: None,
            frames: Arc::new(FrameSlot::default()),
            video: None,
            held_keys: HashSet::new(),
            shown: ShownStats::new(),
        }
    }

    fn monitor(&self) -> Option<MonitorHandle> {
        match &self.extra {
            Some((monitor, _)) => Some(monitor.clone()),
            None => self.window.current_monitor(),
        }
    }

    fn send(&self, event: InputEvent) {
        if let Some(live) = &self.live {
            let _ = live.out.send(ClientMsg::Input(event));
        }
    }

    fn normalized(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let video = self.video?;
        let size = self.window.inner_size();
        Some(input::normalize((x, y), video, (size.width, size.height)))
    }

    fn release_all_keys(&mut self) {
        for usage in std::mem::take(&mut self.held_keys) {
            self.send(InputEvent::Key { usage, pressed: false });
        }
    }

    fn close(&mut self) {
        if let Some(live) = &mut self.live {
            live.disconnecting = true;
            live.conn.close(0u32.into(), b"disconnected by the screen");
        }
    }
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
    displays: BTreeMap<usize, Display>,
    next_view: usize,
    /// The host the main display is connected to, for the extra monitors to join.
    target: Option<Target>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    repaint_at: Option<Instant>,
    modifiers: ModifiersState,
    /// Show the shortcut hint over the video until then.
    hint_until: Option<Instant>,
    /// Keeps this screen on while connected (when the option is set).
    awake: Option<KeepAwake>,
    /// Network link tuned for streaming while connected (Windows Wi-Fi), and its
    /// description for the host.
    link: Option<LinkTuning>,
    network: Option<NetworkInfo>,
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
            displays: BTreeMap::new(),
            next_view: MAIN + 1,
            target: None,
            egui_ctx,
            egui_state: None,
            repaint_at: None,
            modifiers: ModifiersState::empty(),
            hint_until: None,
            awake: None,
            link: None,
            network: None,
            exit_error: None,
        }
    }

    fn streaming(&self) -> bool {
        matches!(self.screen, Screen::Streaming)
    }

    fn main(&self) -> Option<&Display> {
        self.displays.get(&MAIN)
    }

    fn redraw(&self) {
        if let Some(d) = self.main() {
            d.window.request_redraw();
        }
    }

    fn view_of(&self, id: WindowId) -> Option<usize> {
        self.displays.iter().find(|(_, d)| d.window.id() == id).map(|(&view, _)| view)
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let mut attrs = Window::default_attributes().with_title("freemonplay").with_inner_size(PICKER_SIZE);
        if let Some(i) = self.startup.monitor {
            let monitor = event_loop.available_monitors().nth(i);
            match monitor {
                Some(m) => {
                    let p = m.position();
                    attrs = attrs.with_position(PhysicalPosition::new(p.x + 80, p.y + 80));
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
        self.displays.insert(MAIN, Display::new(window, renderer, None));
        if self.startup.all_monitors {
            self.options.extra_monitors = self.other_monitors().into_iter().map(|m| m.key).collect();
        }
        if let Some(spec) = self.startup.host.take() {
            self.connect_address(spec);
        }
        Ok(())
    }

    /// This computer's monitors apart from the one the main window is on.
    fn other_monitors(&self) -> Vec<MonitorRow> {
        let Some(main) = self.main() else { return Vec::new() };
        let current = main.window.current_monitor();
        let mut seen: Vec<String> = Vec::new();
        let mut rows = Vec::new();
        for monitor in main.window.available_monitors() {
            // Windows names monitors like \\.\DISPLAY2; the prefix means nothing to people.
            let name = monitor.name().unwrap_or_else(|| "Monitor".into()).trim_start_matches(r"\\.\").to_string();
            // Two identical monitors can share a name.
            let twins = seen.iter().filter(|n| **n == name).count();
            seen.push(name.clone());
            let key = if twins == 0 { name.clone() } else { format!("{name}#{}", twins + 1) };
            if Some(&monitor) == current.as_ref() {
                continue;
            }
            let size = monitor.size();
            let label = format!("{key} ({}x{})", size.width, size.height);
            rows.push(MonitorRow { key, label, monitor });
        }
        rows
    }

    // ---- Connecting ------------------------------------------------------------------

    fn set_task(&mut self, view: usize, task: JoinHandle<()>) {
        if let Some(d) = self.displays.get_mut(&view) {
            d.task = Some(task);
        }
    }

    fn connect(&mut self, target: Target, code: Option<String>) {
        self.message = None;
        self.screen = Screen::Connecting { to: target.name.clone() };
        let (store, proxy) = (self.store.clone(), self.proxy.clone());
        let task = self.rt.spawn(async move {
            let result = net::prepare(&target, &store, code.clone()).await.map(|p| (p, target)).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { view: MAIN, result, code });
        });
        self.set_task(MAIN, task);
        self.redraw();
    }

    fn connect_address(&mut self, spec: String) {
        self.message = None;
        self.screen = Screen::Connecting { to: spec.clone() };
        let (store, proxy) = (self.store.clone(), self.proxy.clone());
        let code = self.startup.code.take();
        let task = self.rt.spawn(async move {
            let result = async {
                let target = net::resolve(&spec).await?;
                anyhow::Ok((net::prepare(&target, &store, code.clone()).await?, target))
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { view: MAIN, result, code });
        });
        self.set_task(MAIN, task);
        self.redraw();
    }

    fn submit_code(&mut self, target: Target, fingerprint: String, code: String) {
        self.screen = Screen::Connecting { to: target.name.clone() };
        let proxy = self.proxy.clone();
        let task = self.rt.spawn(async move {
            let result = net::prepare_with_code(&target, code.clone(), &fingerprint)
                .await
                .map(|p| (p, target))
                .map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Prepared { view: MAIN, result, code: Some(code) });
        });
        self.set_task(MAIN, task);
        self.redraw();
    }

    fn on_prepared(&mut self, view: usize, result: Result<(Prepared, Target), String>) {
        if view != MAIN {
            match result {
                Ok((Prepared::Ready(connected, credentials), target)) if self.displays.contains_key(&view) => {
                    self.start_session(view, connected, credentials, target);
                }
                Ok((Prepared::Ready(..), _)) => {} // closed meanwhile
                Ok((Prepared::NeedCode { .. }, _)) => self.drop_extra(view, "the host asked to pair again"),
                Err(e) => self.drop_extra(view, &e),
            }
            return;
        }
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
            Ok((Prepared::Ready(connected, credentials), target)) => self.start_session(MAIN, connected, credentials, target),
        }
        self.redraw();
    }

    fn start_session(&mut self, view: usize, connected: net::Connected, credentials: Credentials, target: Target) {
        if view == MAIN {
            self.target = Some(target.clone());
        }
        let Some(display) = self.displays.get(&view) else { return };
        let pairing = matches!(credentials, Credentials::Code(_)).then(|| (target, connected.fingerprint.clone()));
        let request = self.display_request(display);
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let conn = connected.conn.clone();
        let session = net::Session {
            view,
            screen: display.extra.as_ref().map(|(_, id)| id.clone()),
            connected,
            credentials,
            client_name: self.client_name.clone(),
            codecs: match self.options.codec {
                CodecArg::Auto => decode::supported_codecs(),
                CodecArg::H264 => vec![Codec::H264],
                CodecArg::H265 => vec![Codec::H265],
            },
            store: self.store.clone(),
            display: request,
            gpu_adapter: display.renderer.gpu_adapter(),
            outgoing: out_rx,
            outgoing_tx: out_tx.clone(),
            frames: display.frames.clone(),
            proxy: self.proxy.clone(),
        };
        let proxy = self.proxy.clone();
        let task = self.rt.spawn(async move {
            let reason = net::run(session).await.unwrap_or_else(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Closed(view, reason));
        });
        if let Some(display) = self.displays.get_mut(&view) {
            display.task = Some(task);
            display.live = Some(Live { out: out_tx, conn, pairing, connected: false, disconnecting: false });
        }
    }

    /// The virtual display to ask for: the size of the display's monitor (or the windowed
    /// size), at its refresh rate.
    fn display_request(&self, display: &Display) -> DisplayRequest {
        let monitor = display.monitor();
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

    fn on_connected(&mut self, event_loop: &ActiveEventLoop, view: usize, host_name: String, width: u32, height: u32, codec: Codec) {
        let fullscreen = self.options.fullscreen;
        let windowed = self.startup.windowed.unwrap_or(DEFAULT_WINDOWED);
        if view == MAIN
            && let Some(host) = self.main().and_then(|d| d.live.as_ref()).map(|l| l.conn.remote_address().ip().to_canonical())
        {
            (self.link, self.network) = link::tune(host);
        }
        let network = self.network.clone();
        let Some(display) = self.displays.get_mut(&view) else { return };
        tracing::info!("display {view}: connected to {host_name}, receiving {width}x{height} {codec}");
        if let Some(live) = &mut display.live {
            live.connected = true;
            if let Some(network) = network {
                let _ = live.out.send(ClientMsg::Network(network));
            }
        }
        display.video = Some((width, height));
        let w = &display.window;
        match &display.extra {
            Some((_, screen)) => w.set_title(&format!("freemonplay: {host_name} ({})", screen.name)),
            None => w.set_title(&format!("freemonplay: {host_name}")),
        }
        if fullscreen {
            w.set_fullscreen(Some(Fullscreen::Borderless(display.monitor())));
        } else if display.extra.is_none() {
            let _ = w.request_inner_size(PhysicalSize::new(windowed.0, windowed.1));
        }
        // The host draws its cursor into the video; a second local one would be confusing.
        w.set_cursor_visible(false);
        if view != MAIN {
            return;
        }
        self.screen = Screen::Streaming;
        if self.options.keep_awake {
            self.awake = KeepAwake::start();
        }
        self.hint_until = Some(Instant::now() + Duration::from_secs(4));
        self.repaint_at = self.hint_until;
        // Paired by now, so the other monitors can join with the saved pairing.
        self.open_extras(event_loop);
    }

    /// Opens a window on each selected extra monitor and connects it.
    fn open_extras(&mut self, event_loop: &ActiveEventLoop) {
        let Some(target) = self.target.clone() else { return };
        let selected: Vec<MonitorRow> =
            self.other_monitors().into_iter().filter(|m| self.options.extra_monitors.contains(&m.key)).collect();
        for row in selected {
            let view = self.next_view;
            self.next_view += 1;
            let name = row.key.split('#').next().unwrap_or(&row.key).to_string();
            let screen = ScreenId { key: row.key.clone(), name };
            match self.open_extra_window(event_loop, &row.monitor, screen) {
                Ok(display) => {
                    self.displays.insert(view, display);
                }
                Err(e) => {
                    tracing::warn!("couldn't open a window on {}: {e:#}", row.label);
                    continue;
                }
            }
            let (store, proxy, target) = (self.store.clone(), self.proxy.clone(), target.clone());
            let task = self.rt.spawn(async move {
                let result = net::prepare(&target, &store, None).await.map(|p| (p, target)).map_err(|e| format!("{e:#}"));
                let _ = proxy.send_event(UserEvent::Prepared { view, result, code: None });
            });
            self.set_task(view, task);
        }
    }

    /// A window on `monitor`, full screen straight away (black until the video starts).
    fn open_extra_window(&self, event_loop: &ActiveEventLoop, monitor: &MonitorHandle, screen: ScreenId) -> anyhow::Result<Display> {
        let p = monitor.position();
        let mut attrs = Window::default_attributes()
            .with_title(format!("freemonplay ({})", screen.name))
            .with_position(PhysicalPosition::new(p.x + 80, p.y + 80));
        attrs = match self.options.fullscreen {
            true => attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone())))),
            false => {
                let (w, h) = self.startup.windowed.unwrap_or(DEFAULT_WINDOWED);
                attrs.with_inner_size(PhysicalSize::new(w, h))
            }
        };
        let window = Arc::new(event_loop.create_window(attrs)?);
        window.set_cursor_visible(false);
        let renderer = Renderer::new(window.clone(), self.options.vsync)?;
        Ok(Display::new(window, renderer, Some((monitor.clone(), screen))))
    }

    /// Ends an extra monitor's session and closes its window, keeping the others.
    fn drop_extra(&mut self, view: usize, reason: &str) {
        if let Some(mut display) = self.displays.remove(&view) {
            let name = display.extra.as_ref().map_or_else(String::new, |(_, s)| s.name.clone());
            tracing::warn!("{name}: {reason}");
            display.close();
            if let Some(task) = display.task.take()
                && display.live.is_none()
            {
                task.abort();
            }
        }
    }

    fn disconnect(&mut self) {
        for display in self.displays.values_mut() {
            display.close();
        }
    }

    fn on_closed(&mut self, view: usize, reason: String) {
        if view != MAIN {
            // Already gone if we closed it; otherwise the host ended just this screen.
            let disconnecting = self.displays.get(&view).and_then(|d| d.live.as_ref()).is_none_or(|l| l.disconnecting);
            if !disconnecting {
                self.drop_extra(view, &format!("disconnected: {reason}"));
            } else {
                self.displays.remove(&view);
            }
            return;
        }
        let Some(main) = self.displays.get_mut(&MAIN) else { return };
        let Some(live) = main.live.take() else { return };
        main.task = None;
        main.release_all_keys();
        main.video = None;
        main.renderer.clear_video();
        let w = &main.window;
        w.set_fullscreen(None);
        w.set_cursor_visible(true);
        w.set_title("freemonplay");
        let _ = w.request_inner_size(PICKER_SIZE);
        // The other monitors end with the main one.
        let extras: Vec<usize> = self.displays.keys().copied().filter(|&v| v != MAIN).collect();
        for view in extras {
            if let Some(mut display) = self.displays.remove(&view) {
                display.close();
                if let Some(task) = display.task.take()
                    && display.live.is_none()
                {
                    task.abort();
                }
            }
        }
        self.awake = None;
        self.link = None;
        self.network = None;
        self.hint_until = None;
        self.target = None;
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
                if let Some(main) = self.displays.get_mut(&MAIN) {
                    if let Some(task) = main.task.take() {
                        task.abort();
                    }
                    main.live = None;
                }
                self.screen = Screen::Hosts;
            }
        }
        self.redraw();
    }

    // ---- Drawing ---------------------------------------------------------------------

    /// The host list, code dialog or connecting view.
    fn draw_ui(&mut self) {
        let Some(window) = self.main().map(|d| d.window.clone()) else { return };
        let monitors = self.other_monitors();
        let Some(state) = self.egui_state.as_mut() else { return };
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
                    monitors: &monitors,
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

    /// The video, with the shortcut hint on top of the main display for the first few
    /// seconds.
    fn draw_stream(&mut self, view: usize) {
        let hint = view == MAIN && self.hint_until.is_some_and(|t| Instant::now() < t);
        let Some(display) = self.displays.get_mut(&view) else { return };
        let new_frame = display.frames.take();
        if let Some(frame) = &new_frame {
            display.renderer.upload(frame);
        }
        if hint && let Some(state) = self.egui_state.as_mut() {
            let window = display.window.clone();
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
        } else {
            display.renderer.render(None);
        }
        let Some(display) = self.displays.get_mut(&view) else { return };
        // Counted after presenting, so it covers decode, upload and present.
        if let Some(frame) = new_frame {
            display.shown.add(frame.received_at);
        }
        if let Some(stats) = display.shown.take_report()
            && let Some(live) = &display.live
        {
            let _ = live.out.send(ClientMsg::Stats(stats));
        }
    }

    /// Draws egui output on the main display.
    fn present_ui(&mut self, window: &Window, output: egui::FullOutput) {
        if let Some(state) = self.egui_state.as_mut() {
            state.handle_platform_output(window, output.platform_output);
        }
        let primitives = self.egui_ctx.tessellate(output.shapes, output.pixels_per_point);
        if let Some(main) = self.displays.get_mut(&MAIN) {
            main.renderer.render(Some(UiFrame {
                primitives,
                textures: output.textures_delta,
                pixels_per_point: output.pixels_per_point,
            }));
        }
        let delay = output.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        if delay.is_zero() {
            window.request_redraw();
        } else if delay < Duration::from_secs(3600) {
            self.repaint_at = Some(Instant::now() + delay);
        }
    }

    // ---- Input -----------------------------------------------------------------------

    /// Mouse, keyboard and touch while streaming: forwarded to the host screen of the
    /// window they happened in, apart from our own shortcuts.
    fn stream_input(&mut self, event_loop: &ActiveEventLoop, view: usize, event: WindowEvent) {
        let m = self.modifiers;
        let Some(display) = self.displays.get_mut(&view) else { return };
        match event {
            WindowEvent::Focused(false) => display.release_all_keys(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let pressed = event.state == ElementState::Pressed;
                if pressed && m.control_key() && m.alt_key() && m.shift_key() {
                    match code {
                        KeyCode::KeyQ => {
                            self.disconnect();
                            return event_loop.exit();
                        }
                        KeyCode::KeyD => return self.disconnect(),
                        KeyCode::KeyF => {
                            let w = &display.window;
                            let full = w.fullscreen().is_some();
                            w.set_fullscreen((!full).then(|| Fullscreen::Borderless(display.monitor())));
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
                    display.held_keys.insert(usage);
                } else {
                    display.held_keys.remove(&usage);
                }
                display.send(InputEvent::Key { usage, pressed });
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some((x, y)) = display.normalized(position.x, position.y) {
                    display.send(InputEvent::PointerMove { x, y });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(button) = input::button(button) {
                    display.send(InputEvent::PointerButton { button, pressed: state == ElementState::Pressed });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let event = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => InputEvent::Scroll { dx, dy, precise: false },
                    MouseScrollDelta::PixelDelta(p) => {
                        let scale = display.window.scale_factor();
                        InputEvent::Scroll { dx: (p.x / scale) as f32, dy: (p.y / scale) as f32, precise: true }
                    }
                };
                display.send(event);
            }
            WindowEvent::Touch(t) => {
                if let Some((x, y)) = display.normalized(t.location.x, t.location.y) {
                    let phase = match t.phase {
                        winit::event::TouchPhase::Started => TouchPhase::Started,
                        winit::event::TouchPhase::Moved => TouchPhase::Moved,
                        winit::event::TouchPhase::Ended => TouchPhase::Ended,
                        winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
                    };
                    display.send(InputEvent::Touch { id: t.id, phase, x, y });
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.main().is_none()
            && let Err(e) = self.open_window(event_loop)
        {
            self.exit_error = Some(format!("{e:#}"));
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::HostsChanged => {
                if matches!(self.screen, Screen::Hosts) {
                    self.redraw();
                }
            }
            UserEvent::Prepared { view, result, code: _ } => self.on_prepared(view, result),
            UserEvent::Connected { view, host_name, width, height, codec } => {
                self.on_connected(event_loop, view, host_name, width, height, codec)
            }
            // Draw right away: request_redraw would wait for the next display refresh tick,
            // adding up to a frame of latency.
            UserEvent::Frame(view) => {
                if self.streaming() {
                    self.draw_stream(view);
                }
            }
            UserEvent::Closed(view, reason) => self.on_closed(view, reason),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(view) = self.view_of(id) else { return };
        match &event {
            WindowEvent::CloseRequested if view == MAIN => {
                self.disconnect();
                return event_loop.exit();
            }
            WindowEvent::CloseRequested => return self.drop_extra(view, "window closed"),
            WindowEvent::Resized(size) => {
                if let Some(d) = self.displays.get_mut(&view) {
                    d.renderer.resize(size.width, size.height);
                    d.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::RedrawRequested => {
                if self.streaming() || view != MAIN { self.draw_stream(view) } else { self.draw_ui() }
                return;
            }
            _ => {}
        }
        if self.streaming() {
            self.stream_input(event_loop, view, event);
        } else if view == MAIN
            && let (Some(window), Some(state)) = (self.main().map(|d| d.window.clone()), self.egui_state.as_mut())
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
        if self.reports.is_multiple_of(5) {
            tracing::info!("showing {:.0} fps, {:.1} ms from received to shown", stats.shown_fps, stats.display_ms);
        }
        *self = Self { reports: self.reports, ..Self::new() };
        Some(stats)
    }
}

//! The client's screens, drawn with egui: the host list, the pairing code dialog and the
//! connecting view. They only describe what to show and report what the user asked for;
//! app.rs acts on it.

use std::collections::HashMap;

use egui::{Align, Color32, CornerRadius, Key, Layout, RichText, Stroke, Vec2, vec2};

use crate::net::Target;
use crate::{CodecArg, Scale};

/// The brand amber from the app icon.
pub const AMBER: Color32 = Color32::from_rgb(245, 158, 11);
const GRAPHITE: Color32 = Color32::from_rgb(28, 31, 40);
/// Cards and fields, a step lighter than the background.
const CARD: Color32 = Color32::from_rgb(38, 42, 53);
const CARD_EDGE: Color32 = Color32::from_rgb(56, 61, 74);
const MUTED: Color32 = Color32::from_rgb(150, 156, 170);
const ERROR: Color32 = Color32::from_rgb(255, 140, 120);

/// The app icon, embedded (256 px PNG).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

pub enum Screen {
    Hosts,
    Code { target: Target, fingerprint: String, code: String, error: Option<String> },
    Connecting { to: String },
    Streaming,
    /// Keyboard and mouse only: connected, with no video to show.
    Remote { host: String },
}

/// What one of this computer's monitors does while connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Shows one of the host's screens.
    Screen,
    /// Stays this computer's own, controlled by the host's keyboard and mouse.
    KeyboardMouse,
    Unused,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Screen => "Extra screen",
            Role::KeyboardMouse => "Keyboard and mouse only",
            Role::Unused => "Not used",
        }
    }

    fn explanation(self) -> &'static str {
        match self {
            Role::Screen => "Shows another screen for the computer you connect to.",
            Role::KeyboardMouse => "Stays this computer's own; the other computer's keyboard and mouse can move onto it.",
            Role::Unused => "Left alone.",
        }
    }
}

pub struct HostRow {
    /// Position in the full host list (what `Action::Connect` refers to).
    pub index: usize,
    pub name: String,
    pub address: String,
    pub paired: bool,
}

/// One of this computer's other monitors, offered as an extra screen.
pub struct MonitorRow {
    /// Stable name, used to remember the choice and as the screen's identity on the host.
    pub key: String,
    pub label: String,
    pub monitor: winit::monitor::MonitorHandle,
}

/// Settings for the next connection.
pub struct Options {
    pub fullscreen: bool,
    pub scale: Scale,
    pub vsync: bool,
    pub codec: CodecArg,
    /// Stop this screen dimming, locking or sleeping while connected.
    pub keep_awake: bool,
    /// What the monitor this window is on does.
    pub main_role: Role,
    /// What the other monitors do (by `MonitorRow::key`; missing means unused).
    pub roles: HashMap<String, Role>,
    /// Keyboard and mouse mode: Command acts as Ctrl, so Mac shortcuts work.
    pub swap_command_control: bool,
    /// Share clipboard text with the host both ways.
    pub share_clipboard: bool,
}

impl Options {
    pub fn role(&self, key: &str) -> Role {
        self.roles.get(key).copied().unwrap_or(Role::Unused)
    }

    /// Whether any monitor takes the host's keyboard and mouse.
    pub fn any_keyboard_mouse(&self) -> bool {
        self.main_role == Role::KeyboardMouse || self.roles.values().any(|&r| r == Role::KeyboardMouse)
    }
}

pub enum Action {
    Connect(usize),
    ConnectAddress(String),
    SubmitCode,
    CancelCode,
    CancelConnect,
    Disconnect,
}

/// Graphite and amber, matching the app icon, in dark mode.
pub fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.visuals.panel_fill = GRAPHITE;
        s.visuals.window_fill = GRAPHITE;
        s.visuals.extreme_bg_color = CARD;
        // Selected choices look like the primary button: amber, dark text.
        s.visuals.selection.bg_fill = AMBER;
        s.visuals.selection.stroke = Stroke::new(1.0, GRAPHITE);
        s.visuals.hyperlink_color = AMBER;
        s.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
        s.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
        s.visuals.widgets.active.corner_radius = CornerRadius::same(6);
        s.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, AMBER.linear_multiply(0.7));
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
    });
}

/// The app icon as a texture, made once.
pub fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (w, h, rgba) = icon_rgba()?;
    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    Some(ctx.load_texture("logo", image, egui::TextureOptions::LINEAR))
}

/// The app icon's pixels (for the window icon too).
pub fn icon_rgba() -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(ICON_PNG)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba).then(|| (info.width, info.height, buf[..info.buffer_size()].to_vec()))
}

/// Keyboard shortcut text with this OS's key names (Control, Option and Shift on a Mac).
pub fn shortcut(key: char) -> String {
    if cfg!(target_os = "macos") { format!("Control+Option+Shift+{key}") } else { format!("Ctrl+Alt+Shift+{key}") }
}

pub struct View<'a> {
    pub screen: &'a mut Screen,
    pub hosts: &'a [HostRow],
    pub monitors: &'a [MonitorRow],
    /// Whether this OS can take the host's keyboard and mouse yet.
    pub kvm_supported: bool,
    pub message: Option<&'a str>,
    pub address: &'a mut String,
    pub options: &'a mut Options,
    pub logo: Option<&'a egui::TextureHandle>,
}

pub fn draw(ui: &mut egui::Ui, view: View<'_>) -> Option<Action> {
    let mut action = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(GRAPHITE).inner_margin(egui::Margin::symmetric(28, 24)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                header(ui, view.logo);
                ui.add_space(14.0);
                if let Some(message) = view.message {
                    notice(ui, message);
                    ui.add_space(6.0);
                }
                action = match view.screen {
                    Screen::Hosts => hosts(ui, view.hosts, view.monitors, view.kvm_supported, view.address, view.options),
                    Screen::Code { target, code, error, .. } => code_dialog(ui, &target.name, code, error.as_deref()),
                    Screen::Connecting { to } => connecting(ui, to),
                    Screen::Streaming => None,
                    Screen::Remote { host } => remote(ui, host),
                };
            });
        });
    action
}

fn header(ui: &mut egui::Ui, logo: Option<&egui::TextureHandle>) {
    ui.horizontal(|ui| {
        if let Some(logo) = logo {
            ui.add(egui::Image::new((logo.id(), Vec2::splat(56.0))).corner_radius(12.0));
        }
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("freemonplay").size(24.0).strong().color(Color32::WHITE));
            ui.label(RichText::new("Use this screen as an extra display for a computer on your network.").color(MUTED));
        });
    });
}

/// A highlighted message (errors, disconnections).
fn notice(ui: &mut egui::Ui, message: &str) {
    egui::Frame::new()
        .fill(Color32::from_rgb(70, 45, 20))
        .stroke(Stroke::new(1.0, AMBER.linear_multiply(0.5)))
        .corner_radius(8.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(message).color(Color32::from_rgb(255, 214, 160)));
        });
}

/// A rounded panel for a group of rows.
fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, CARD_EDGE))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        })
        .inner
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).size(15.0).strong().color(Color32::WHITE));
}

/// The main action on a screen: amber.
fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).strong().color(GRAPHITE))
        .fill(if enabled { AMBER } else { AMBER.linear_multiply(0.35) })
        .min_size(vec2(96.0, 32.0));
    ui.add_enabled(enabled, button).clicked()
}

/// A small screen outline, as in the logo's corners.
fn screen_glyph(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(30.0, 26.0), egui::Sense::hover());
    let stroke = Stroke::new(1.8, color);
    let screen = egui::Rect::from_min_size(rect.min + vec2(2.0, 3.0), vec2(26.0, 16.0));
    let p = ui.painter();
    p.rect_stroke(screen, 3.0, stroke, egui::StrokeKind::Inside);
    let (x, y) = (screen.center().x, screen.bottom());
    p.line_segment([egui::pos2(x, y), egui::pos2(x, y + 4.0)], stroke);
    p.line_segment([egui::pos2(x - 6.0, y + 4.5), egui::pos2(x + 6.0, y + 4.5)], stroke);
}

fn hosts(
    ui: &mut egui::Ui,
    hosts: &[HostRow],
    monitors: &[MonitorRow],
    kvm_supported: bool,
    address: &mut String,
    options: &mut Options,
) -> Option<Action> {
    let mut action = None;
    section(ui, "Connect to a computer");
    card(ui, |ui| {
        if hosts.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for computers running freemonplay…");
            });
            ui.label(
                RichText::new(
                    "Open freemonplay on the computer you want more screens for. If it doesn't show up, a firewall may \
                     be blocking discovery: use Connect by address below.",
                )
                .color(MUTED)
                .small(),
            );
            ui.add_space(4.0);
        }
        for (i, host) in hosts.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            let i = host.index;
            ui.horizontal(|ui| {
                screen_glyph(ui, if host.paired { AMBER } else { MUTED });
                ui.vertical(|ui| {
                    ui.label(RichText::new(&host.name).size(15.0).strong().color(Color32::WHITE));
                    let status = if host.paired { "Paired" } else { "Not paired yet: you'll enter its code once" };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&host.address).color(MUTED).small());
                        ui.label(RichText::new("·").color(MUTED).small());
                        ui.label(RichText::new(status).color(if host.paired { AMBER } else { MUTED }).small());
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary(ui, "Connect", true) {
                        action = Some(Action::Connect(i));
                    }
                });
            });
        }
    });

    monitor_roles(ui, monitors, kvm_supported, options);

    ui.add_space(6.0);
    egui::CollapsingHeader::new("Connect by address").show(ui, |ui| {
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(address)
                    .hint_text("192.168.1.20 or a computer name")
                    .desired_width(240.0)
                    .margin(vec2(8.0, 6.0)),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let ready = !address.trim().is_empty();
            if (primary(ui, "Connect", ready) || enter) && ready {
                action = Some(Action::ConnectAddress(address.trim().to_string()));
            }
        });
    });
    egui::CollapsingHeader::new("Options").show(ui, |ui| options_panel(ui, options));

    ui.add_space(10.0);
    ui.label(
        RichText::new(format!(
            "While connected: {} disconnects · {} full screen on or off · {} quits",
            shortcut('D'),
            shortcut('F'),
            shortcut('Q')
        ))
        .color(MUTED)
        .small(),
    );
    ui.label(RichText::new(format!("freemonplay {}", env!("CARGO_PKG_VERSION"))).color(MUTED.linear_multiply(0.7)).small());
    action
}

/// How this computer's monitors are used. Hidden when there's nothing to choose.
fn monitor_roles(ui: &mut egui::Ui, monitors: &[MonitorRow], kvm_supported: bool, options: &mut Options) {
    if monitors.is_empty() && !kvm_supported {
        return;
    }
    ui.add_space(6.0);
    if monitors.is_empty() {
        // One monitor: a simple either/or.
        section(ui, "Use this screen as");
        card(ui, |ui| {
            ui.horizontal(|ui| {
                for role in [Role::Screen, Role::KeyboardMouse] {
                    ui.selectable_value(&mut options.main_role, role, RichText::new(role.label()).size(14.0));
                }
            });
            ui.label(RichText::new(options.main_role.explanation()).color(MUTED).small());
        });
    } else {
        section(ui, "Your monitors");
        card(ui, |ui| {
            egui::Grid::new("monitors").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("This one (with this window)");
                let choices: &[Role] = if kvm_supported { &[Role::Screen, Role::KeyboardMouse] } else { &[Role::Screen] };
                role_picker(ui, "main", &mut options.main_role, choices);
                ui.end_row();
                for m in monitors {
                    ui.label(&m.label);
                    let mut role = options.role(&m.key);
                    // Extra screens join the main window's connection, so it must stream too.
                    let mut choices = vec![Role::Unused];
                    if options.main_role == Role::Screen {
                        choices.push(Role::Screen);
                    }
                    if kvm_supported {
                        choices.push(Role::KeyboardMouse);
                    }
                    if !choices.contains(&role) {
                        role = Role::Unused;
                    }
                    role_picker(ui, &m.key, &mut role, &choices);
                    options.roles.insert(m.key.clone(), role);
                    ui.end_row();
                }
            });
            ui.label(
                RichText::new("Extra screen: shows another screen for the other computer. Keyboard and mouse only: stays this computer's, and the other computer's keyboard and mouse can move onto it.")
                    .color(MUTED)
                    .small(),
            );
        });
    }
    // A Mac controlling a Mac needs no swapping.
    if options.any_keyboard_mouse() && !cfg!(target_os = "macos") {
        ui.checkbox(&mut options.swap_command_control, "Mac shortcuts: Command acts as Ctrl")
            .on_hover_text("So Command+C copies here too. Ctrl then acts as the Windows key.");
    }
}

fn options_panel(ui: &mut egui::Ui, options: &mut Options) {
    ui.checkbox(&mut options.fullscreen, "Full screen (on the monitor this window is on)");
    egui::Grid::new("options").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Sharpness");
        egui::ComboBox::from_id_salt("sharpness")
            .selected_text(match options.scale {
                Scale::Auto => "Automatic",
                Scale::One => "Standard (1x)",
                Scale::Two => "Retina (2x)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut options.scale, Scale::Auto, "Automatic");
                ui.selectable_value(&mut options.scale, Scale::One, "Standard (1x)");
                ui.selectable_value(&mut options.scale, Scale::Two, "Retina (2x)");
            });
        ui.end_row();
        ui.label("Video codec");
        egui::ComboBox::from_id_salt("codec")
            .selected_text(match options.codec {
                CodecArg::Auto => "Automatic",
                CodecArg::H264 => "H.264",
                CodecArg::H265 => "HEVC",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut options.codec, CodecArg::Auto, "Automatic");
                ui.selectable_value(&mut options.codec, CodecArg::H264, "H.264");
                ui.selectable_value(&mut options.codec, CodecArg::H265, "HEVC");
            });
        ui.end_row();
    });
    ui.checkbox(&mut options.keep_awake, "Keep this screen awake while connected");
    ui.checkbox(&mut options.share_clipboard, "Share the clipboard with the other computer")
        .on_hover_text("Text and images copied on either computer can be pasted on the other. Passwords copied from password managers stay put.");
    ui.checkbox(&mut options.vsync, "Wait for the display refresh (no tearing)")
        .on_hover_text("Turning this off saves up to one refresh of latency, but the picture can tear.");
}

fn role_picker(ui: &mut egui::Ui, id: &str, role: &mut Role, choices: &[Role]) {
    egui::ComboBox::from_id_salt(("role", id)).width(200.0).selected_text(role.label()).show_ui(ui, |ui| {
        for &choice in choices {
            ui.selectable_value(role, choice, choice.label()).on_hover_text(choice.explanation());
        }
    });
}

/// Keyboard and mouse only: nothing to show but how to use it.
fn remote(ui: &mut egui::Ui, host: &str) -> Option<Action> {
    let mut action = None;
    section(ui, &format!("Connected to {host}"));
    card(ui, |ui| {
        ui.label(
            "Its keyboard and mouse can now move onto this computer's monitors: push its pointer off the side of \
             its main display that faces this one (chosen in freemonplay's menu there). Moving back returns them.",
        );
        ui.label(RichText::new(format!("{} on that computer brings them back from anywhere.", shortcut('K'))).color(MUTED));
        ui.label(RichText::new("Text and images you copy on one computer can be pasted on the other.").color(MUTED));
    });
    ui.add_space(8.0);
    if primary(ui, "Disconnect", true) {
        action = Some(Action::Disconnect);
    }
    action
}

fn code_dialog(ui: &mut egui::Ui, host: &str, code: &mut String, error: Option<&str>) -> Option<Action> {
    let mut action = None;
    section(ui, &format!("Pair with {host}"));
    card(ui, |ui| {
        ui.label("Enter the 6 digit pairing code shown in freemonplay's menu on that computer.");
        ui.label(RichText::new("You only need to do this once.").color(MUTED).small());
        ui.add_space(6.0);
        let field = ui.add(
            egui::TextEdit::singleline(code)
                .hint_text(RichText::new("000 000").font(egui::FontId::monospace(30.0)).color(MUTED.linear_multiply(0.6)))
                .char_limit(7)
                .font(egui::FontId::monospace(30.0))
                .horizontal_align(Align::Center)
                .desired_width(200.0)
                .margin(vec2(10.0, 8.0)),
        );
        // Digits only (one space allowed, as in "123 456").
        code.retain(|c| c.is_ascii_digit() || c == ' ');
        if !field.has_focus() && !field.lost_focus() {
            field.request_focus();
        }
        if let Some(error) = error {
            ui.label(RichText::new(error).color(ERROR));
        }
        let digits = code.chars().filter(char::is_ascii_digit).count();
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if primary(ui, "Pair", digits == 6) || (enter && digits == 6) {
                action = Some(Action::SubmitCode);
            }
            if ui.button("Cancel").clicked() {
                action = Some(Action::CancelCode);
            }
        });
    });
    action
}

fn connecting(ui: &mut egui::Ui, to: &str) -> Option<Action> {
    let mut action = None;
    card(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(format!("Connecting to {to}…")).size(15.0));
        });
        ui.add_space(4.0);
        if ui.button("Cancel").clicked() {
            action = Some(Action::CancelConnect);
        }
    });
    action
}

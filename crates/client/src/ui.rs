//! The client's screens, drawn with egui: the host list, the pairing code dialog and the
//! connecting view. They only describe what to show and report what the user asked for;
//! app.rs acts on it.

use std::collections::HashMap;

use egui::{Align, Color32, Key, Layout, RichText};

use crate::net::Target;
use crate::{CodecArg, Scale};

/// The brand amber from the app icon.
pub const AMBER: Color32 = Color32::from_rgb(245, 158, 11);
const GRAPHITE: Color32 = Color32::from_rgb(28, 31, 40);

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
            Role::Screen => "Screen for the host",
            Role::KeyboardMouse => "Keyboard and mouse",
            Role::Unused => "Not used",
        }
    }
}

pub struct HostRow {
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
        s.visuals.selection.bg_fill = AMBER.linear_multiply(0.6);
        s.visuals.hyperlink_color = AMBER;
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
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
}

pub fn draw(ui: &mut egui::Ui, view: View<'_>) -> Option<Action> {
    let mut action = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(GRAPHITE).inner_margin(24.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("f").size(30.0).strong().color(AMBER));
                ui.vertical(|ui| {
                    ui.label(RichText::new("freemonplay").size(20.0).strong());
                    ui.label(RichText::new("Use this screen as an extra display for a computer on your network.").weak());
                });
            });
            ui.add_space(12.0);
            if let Some(message) = view.message {
                egui::Frame::new()
                    .fill(Color32::from_rgb(70, 45, 20))
                    .corner_radius(6.0)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(message).color(Color32::from_rgb(255, 214, 160)));
                    });
                ui.add_space(8.0);
            }
            action = match view.screen {
                Screen::Hosts => hosts(ui, view.hosts, view.monitors, view.kvm_supported, view.address, view.options),
                Screen::Code { target, code, error, .. } => code_dialog(ui, &target.name, code, error.as_deref()),
                Screen::Connecting { to } => connecting(ui, to),
                Screen::Streaming => None,
                Screen::Remote { host } => remote(ui, host),
            };
        });
    action
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
    ui.label(RichText::new("Computers on your network").strong());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if hosts.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for computers running freemonplay…");
            });
            ui.label(
                RichText::new("Start freemonplay on the computer you want to extend. Firewalls can block discovery; you can also connect by address below.")
                    .weak()
                    .small(),
            );
        }
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            for (i, host) in hosts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&host.name).strong());
                            if host.paired {
                                ui.label(RichText::new("paired").small().color(AMBER));
                            }
                        });
                        ui.label(RichText::new(&host.address).weak().small());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Connect").clicked() {
                            action = Some(Action::Connect(i));
                        }
                    });
                });
                ui.separator();
            }
        });
    });

    if !monitors.is_empty() || kvm_supported {
        ui.add_space(4.0);
        ui.label(RichText::new("Monitors").strong()).on_hover_text(
            "Screen: shows another screen for the host. Keyboard and mouse: stays this computer's, \
             and the host's keyboard and mouse can move onto it.",
        );
        egui::Grid::new("monitors").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("This monitor (the window's)");
            let choices: &[Role] = if kvm_supported { &[Role::Screen, Role::KeyboardMouse] } else { &[Role::Screen] };
            role_picker(ui, "main", &mut options.main_role, choices);
            ui.end_row();
            for m in monitors {
                ui.label(&m.label);
                let mut role = options.role(&m.key);
                // A screen needs the main window streaming too (the extra screens join it).
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
        // A Mac controlling a Mac needs no swapping.
        if options.any_keyboard_mouse() && !cfg!(target_os = "macos") {
            ui.checkbox(&mut options.swap_command_control, "Mac shortcuts: Command acts as Ctrl")
                .on_hover_text("So Command+C copies here too. Ctrl then acts as the Windows key.");
        }
    }

    ui.add_space(4.0);
    egui::CollapsingHeader::new("Connect by address").show(ui, |ui| {
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(address).hint_text("192.168.1.20 or name").desired_width(220.0));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.add_enabled(!address.trim().is_empty(), egui::Button::new("Connect")).clicked() || enter)
                && !address.trim().is_empty()
            {
                action = Some(Action::ConnectAddress(address.trim().to_string()));
            }
        });
    });
    egui::CollapsingHeader::new("Options").show(ui, |ui| {
        ui.checkbox(&mut options.fullscreen, "Full screen (on the monitor this window is on)");
        egui::ComboBox::from_label("Sharpness")
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
        egui::ComboBox::from_label("Video codec")
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
        ui.checkbox(&mut options.keep_awake, "Keep this screen awake while connected");
        ui.checkbox(&mut options.share_clipboard, "Share the clipboard with the host")
            .on_hover_text("Text copied on either computer can be pasted on the other. Passwords copied from password managers stay put.");
        ui.checkbox(&mut options.vsync, "Wait for the display refresh (no tearing)")
            .on_hover_text("Turning this off saves up to one refresh of latency, but the picture can tear.");
    });

    ui.add_space(8.0);
    ui.label(
        RichText::new("While connected: Ctrl+Alt+Shift+D disconnects, Ctrl+Alt+Shift+F toggles full screen, Ctrl+Alt+Shift+Q quits.")
            .weak()
            .small(),
    );
    action
}

fn role_picker(ui: &mut egui::Ui, id: &str, role: &mut Role, choices: &[Role]) {
    egui::ComboBox::from_id_salt(("role", id)).selected_text(role.label()).show_ui(ui, |ui| {
        for &choice in choices {
            ui.selectable_value(role, choice, choice.label());
        }
    });
}

/// Keyboard and mouse only: nothing to show but how to use it.
fn remote(ui: &mut egui::Ui, host: &str) -> Option<Action> {
    let mut action = None;
    ui.label(RichText::new(format!("Connected to {host}")).strong().size(16.0));
    ui.label(
        "Its keyboard and mouse can move onto this computer's monitors: push its pointer off the \
         side of its main display that faces them (set in freemonplay's menu there). Moving back \
         returns them; Ctrl+Alt+Shift+K on the host brings them back from anywhere.",
    );
    ui.label(RichText::new("Text you copy on one computer can be pasted on the other.").weak());
    ui.add_space(6.0);
    if ui.button("Disconnect").clicked() {
        action = Some(Action::Disconnect);
    }
    action
}

fn code_dialog(ui: &mut egui::Ui, host: &str, code: &mut String, error: Option<&str>) -> Option<Action> {
    let mut action = None;
    ui.label(RichText::new(format!("Pair with {host}")).strong().size(16.0));
    ui.label("Enter the 6 digit code shown in freemonplay's menu on that computer (or in its terminal).");
    if let Some(error) = error {
        ui.label(RichText::new(error).color(Color32::from_rgb(255, 140, 120)));
    }
    ui.add_space(6.0);
    let field = ui.add(
        egui::TextEdit::singleline(code)
            .hint_text("000000")
            .char_limit(7)
            .font(egui::TextStyle::Heading)
            .desired_width(140.0),
    );
    // Digits only (one space allowed, as in "123 456").
    code.retain(|c| c.is_ascii_digit() || c == ' ');
    if !field.has_focus() && !field.lost_focus() {
        field.request_focus();
    }
    let digits = code.chars().filter(char::is_ascii_digit).count();
    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    ui.horizontal(|ui| {
        if ui.add_enabled(digits == 6, egui::Button::new("Pair")).clicked() || (enter && digits == 6) {
            action = Some(Action::SubmitCode);
        }
        if ui.button("Cancel").clicked() {
            action = Some(Action::CancelCode);
        }
    });
    ui.label(RichText::new("You only need to do this once per computer.").weak().small());
    action
}

fn connecting(ui: &mut egui::Ui, to: &str) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(format!("Connecting to {to}…"));
    });
    if ui.button("Cancel").clicked() {
        action = Some(Action::CancelConnect);
    }
    action
}

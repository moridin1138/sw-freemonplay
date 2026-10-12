//! Freemonplay client: turns this screen into an extra monitor for a Freemonplay host.
//!
//! Opens a window listing the hosts on the network; connecting fills the monitor the
//! window is on, plus any other monitors picked under "Also use these monitors" (each
//! becomes a separate screen on the host). Shortcuts while connected (not forwarded to the host):
//!   Ctrl+Alt+Shift+D  disconnect (back to the list)
//!   Ctrl+Alt+Shift+F  toggle full screen
//!   Ctrl+Alt+Shift+Q  quit

mod app;
mod awake;
mod clip;
mod decode;
mod input;
mod kvm;
mod link;
mod net;
mod render;
#[cfg(target_os = "windows")]
mod render_d3d12;
#[cfg(target_os = "macos")]
mod render_metal;
mod selftest;
mod store;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use winit::event_loop::EventLoop;

pub use app::UserEvent;
use store::ClientStore;

// The app's text in each language: one file per language in the repository's locales
// folder, shared with the host.
rust_i18n::i18n!("../../locales", fallback = "en");

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Connect straight to this host (name, IP, or IP:port) instead of showing the list.
    #[arg(long)]
    host: Option<String>,
    /// Pairing code shown by the host, to skip the pairing dialog the first time.
    #[arg(long)]
    code: Option<String>,
    /// Which monitor to open on (0 = first). Connecting fills the monitor the window is on.
    #[arg(long)]
    monitor: Option<usize>,
    /// Use all of this computer's monitors, each as a separate screen on the host.
    #[arg(long)]
    all_monitors: bool,
    /// Keyboard and mouse only: no screens; the host's keyboard and mouse control all of
    /// this computer's monitors (Windows for now).
    #[arg(long, conflicts_with = "all_monitors")]
    keyboard_mouse: bool,
    /// Stream in a window of this size (e.g. 1280x800) instead of full screen.
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
    /// Let this screen dim, lock or sleep while connected (by default it's kept awake).
    #[arg(long)]
    allow_sleep: bool,
    /// Don't share clipboard text with the host.
    #[arg(long)]
    no_clipboard: bool,
    /// Name this screen shows up as on the host. Defaults to this computer's hostname.
    #[arg(long)]
    name: Option<String>,
    /// Where to keep this client's id and paired hosts.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Check that this machine's video decoder and GPU display work (no window needed),
    /// then exit.
    #[arg(long)]
    self_test: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum, serde::Serialize, serde::Deserialize)]
pub enum Scale {
    #[default]
    Auto,
    #[value(name = "1")]
    One,
    #[value(name = "2")]
    Two,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, ValueEnum, serde::Serialize, serde::Deserialize)]
pub enum CodecArg {
    #[default]
    Auto,
    H264,
    H265,
}

fn main() -> Result<()> {
    init_logging();
    set_language();
    let args = Args::parse();
    #[cfg(target_os = "windows")]
    hide_own_console();
    if args.self_test {
        return selftest::run();
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let windowed = args.windowed.as_deref().map(parse_size).transpose()?;

    let rt = tokio::runtime::Runtime::new()?;
    let store = Arc::new(Mutex::new(ClientStore::open(args.config_dir.clone())?));
    let saved = store.lock().unwrap().settings();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let hosts = {
        let proxy = proxy.clone();
        net::browse(move || {
            let _ = proxy.send_event(UserEvent::HostsChanged);
        })?
    };

    let mut app = app::App::new(
        rt.handle().clone(),
        proxy,
        store,
        hosts,
        args.name.clone().unwrap_or_else(hostname),
        app::Startup {
            monitor: args.monitor,
            all_monitors: args.all_monitors,
            keyboard_mouse: args.keyboard_mouse,
            windowed,
            host: args.host,
            code: args.code,
        },
        // The options as last set in the window, with command line flags winning.
        ui::Options {
            fullscreen: windowed.is_none() && saved.fullscreen,
            scale: if args.scale == Scale::Auto { saved.scale } else { args.scale },
            vsync: saved.vsync && !args.no_vsync,
            codec: if args.codec == CodecArg::Auto { saved.codec } else { args.codec },
            keep_awake: saved.keep_awake && !args.allow_sleep,
            main_role: ui::Role::Screen,
            roles: Default::default(),
            swap_command_control: true,
            share_clipboard: saved.share_clipboard && !args.no_clipboard,
            toolbar: saved.toolbar,
        },
    );
    event_loop.run_app(&mut app)?;
    rt.shutdown_timeout(Duration::from_millis(300));
    match app.exit_error {
        Some(e) => bail!(e),
        None => Ok(()),
    }
}

/// Started from Explorer or the Start menu, Windows gives this console program a console
/// window of its own, which would sit behind the app showing log lines. Let it go. Started
/// from a terminal, the console is shared and output still appears there.
#[cfg(target_os = "windows")]
fn hide_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut processes = [0u32; 2];
    // SAFETY: fills at most two entries of our buffer.
    if unsafe { GetConsoleProcessList(&mut processes) } == 1 {
        // SAFETY: detaching from our own console.
        let _ = unsafe { FreeConsole() };
    }
}

/// Where the client logs: ~/Library/Logs/freemonplay on a Mac, %LOCALAPPDATA%\freemonplay
/// on Windows, ~/.local/state/freemonplay on Linux.
fn log_path() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let dir = if cfg!(target_os = "macos") {
        var("HOME")?.join("Library/Logs/freemonplay")
    } else if cfg!(target_os = "windows") {
        var("LOCALAPPDATA")?.join("freemonplay")
    } else {
        var("XDG_STATE_HOME").or_else(|| var("HOME").map(|h| h.join(".local/state")))?.join("freemonplay")
    };
    Some(dir.join("client.log"))
}

/// Logs to the terminal (if any) and to a file, which the app's "Show log" opens. The file
/// starts over (keeping the previous one as client.old.log) once it passes 5 MB.
fn init_logging() {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu=warn,wgpu_core=error,wgpu_hal=error,naga=warn,quinn=warn,mdns_sd=warn".into());
    let file = log_path().and_then(|path| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20) {
            let _ = std::fs::rename(&path, path.with_file_name("client.old.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()
    });
    let file_layer = file.map(|f| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(Mutex::new(f)));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();
}

/// Uses the OS's language when the app has it, else English. `FMP_LANG` overrides it
/// (e.g. `FMP_LANG=de`), for trying translations.
fn set_language() {
    let wanted = std::env::var("FMP_LANG").ok().or_else(sys_locale::get_locale).unwrap_or_default();
    let available = rust_i18n::available_locales!();
    let available: Vec<&str> = available.iter().map(|l| l.as_ref()).collect();
    let language = fmp_proto::language::pick(&wanted, &available);
    tracing::info!("language: {language} (system: {wanted})");
    rust_i18n::set_locale(language);
}

/// Opens the log file with the usual program for text.
pub fn open_log() {
    let Some(path) = log_path() else { return };
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(program).arg(&path).spawn() {
        tracing::error!("couldn't open {}: {e}", path.display());
    }
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').context("expected WxH, e.g. 1280x800")?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Freemonplay Screen".into())
}

#[cfg(test)]
mod tests {

    /// Keys in a locale file (`key: "text"` lines).
    fn keys(text: &str) -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("_version") && !l.trim().is_empty())
            .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
            .collect()
    }

    #[test]
    fn translations_match_english() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales");
        let english = keys(&std::fs::read_to_string(dir.join("en.yml")).unwrap());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            for key in keys(&text) {
                assert!(english.contains(&key), "{name}: {key} isn't in en.yml");
            }
            // English placeholders must survive translation.
            for line in text.lines().filter(|l| l.contains(": \"")) {
                let (key, value) = line.split_once(':').unwrap();
                let en_line = std::fs::read_to_string(dir.join("en.yml")).unwrap();
                let original = en_line.lines().find(|l| l.split_once(':').is_some_and(|(k, _)| k == key)).unwrap_or_default();
                for placeholder in original.split("%{").skip(1).filter_map(|p| p.split_once('}')).map(|(p, _)| p) {
                    assert!(value.contains(&format!("%{{{placeholder}}}")), "{name}: {key} lost %{{{placeholder}}}");
                }
            }
        }
    }
}

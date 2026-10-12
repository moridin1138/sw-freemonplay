//! freemonplay client: turns this screen into an extra monitor for a freemonplay host.
//!
//! Opens a window listing the hosts on the network; connecting fills the monitor the
//! window is on, plus any other monitors picked under "Also use these monitors" (each
//! becomes a separate screen on the host). Shortcuts while connected (not forwarded to the host):
//!   Ctrl+Alt+Shift+D  disconnect (back to the list)
//!   Ctrl+Alt+Shift+F  toggle full screen
//!   Ctrl+Alt+Shift+Q  quit

mod app;
mod awake;
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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scale {
    Auto,
    #[value(name = "1")]
    One,
    #[value(name = "2")]
    Two,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CodecArg {
    Auto,
    H264,
    H265,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu=warn,wgpu_core=error,wgpu_hal=error,naga=warn,quinn=warn,mdns_sd=warn".into()),
        )
        .init();
    let args = Args::parse();
    if args.self_test {
        return selftest::run();
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let windowed = args.windowed.as_deref().map(parse_size).transpose()?;

    let rt = tokio::runtime::Runtime::new()?;
    let store = Arc::new(Mutex::new(ClientStore::open(args.config_dir.clone())?));
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
        ui::Options {
            fullscreen: windowed.is_none(),
            scale: args.scale,
            vsync: !args.no_vsync,
            codec: args.codec,
            keep_awake: !args.allow_sleep,
            main_role: ui::Role::Screen,
            roles: Default::default(),
            swap_command_control: true,
        },
    );
    event_loop.run_app(&mut app)?;
    rt.shutdown_timeout(Duration::from_millis(300));
    match app.exit_error {
        Some(e) => bail!(e),
        None => Ok(()),
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
        .unwrap_or_else(|| "freemonplay client".into())
}

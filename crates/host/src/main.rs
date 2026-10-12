//! freemonplay host: advertises itself on the LAN, and for every client that connects it
//! creates a virtual display matching the client's screen and streams it there.

// Only macOS hosts exist so far: on other OSes the menu bar's session/settings APIs and the
// capture helpers have no callers yet.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

mod clipboard;
mod encoder;
mod frame;
mod kvm;
#[cfg(target_os = "macos")]
mod menubar;
mod pattern;
mod platform;
mod rate;
mod registry;
mod session;
mod settings;
mod store;

use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use fmp_proto::{ALPN, DEFAULT_PORT, PROTOCOL_VERSION, SERVICE_TYPE};

use session::{Ctx, Overrides};
use store::HostStore;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// UDP port to listen on.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Name shown to clients. Defaults to this computer's hostname.
    #[arg(long)]
    name: Option<String>,
    /// Frame rate cap (default 60). Each client also caps it at its own refresh rate.
    /// Overrides the saved setting for this run.
    #[arg(long)]
    max_fps: Option<u32>,
    /// Video bitrate in Mbit/s. Defaults to a value based on resolution and frame rate.
    #[arg(long)]
    bitrate_mbps: Option<f64>,
    /// Video encoder.
    #[arg(long, value_enum, default_value_t = encoder::EncoderChoice::Auto)]
    encoder: encoder::EncoderChoice,
    /// Video codec (default auto: the client's preference, HEVC when it can decode it in
    /// hardware, as long as this host can encode it). Overrides the saved setting.
    #[arg(long, value_enum)]
    codec: Option<encoder::CodecChoice>,
    /// Ignore mouse, keyboard and touch from clients (view only).
    #[arg(long)]
    no_input: bool,
    /// Don't advertise on the LAN; clients must use --host.
    #[arg(long)]
    no_mdns: bool,
    /// Forget every paired client; they'll need the pairing code again.
    #[arg(long)]
    forget_clients: bool,
    /// Where to keep the host identity and paired clients.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Diagnostic: stream a generated test picture instead of creating virtual displays.
    /// Needs no permissions and works on any OS.
    #[arg(long)]
    test_pattern: bool,
    /// Run as a menu bar app. This is the default when launched as freemonplay.app.
    #[arg(long, conflicts_with = "no_menubar")]
    menubar: bool,
    /// Run in the terminal even when launched from inside freemonplay.app.
    #[arg(long)]
    no_menubar: bool,
    /// Diagnostic: create a virtual display for a few seconds. Examples: 1920x1080,
    /// 2732x2048@2x (HiDPI), 1920x1080:30 (30 Hz).
    #[arg(long, value_name = "WxH")]
    probe_display: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let gui = cfg!(target_os = "macos") && !args.no_menubar && (args.menubar || launched_from_app_bundle());
    init_logging(gui)?;
    let _ = rustls::crypto::ring::default_provider().install_default();

    if let Some(size) = &args.probe_display {
        return probe_display(size);
    }
    if args.test_pattern {
        tracing::warn!("test pattern mode: clients get a generated picture, not a virtual display");
    } else {
        platform::check_permissions()?;
    }

    let store = HostStore::open(args.config_dir.clone())?;
    if args.forget_clients {
        store.forget_all()?;
        tracing::info!("forgot all paired clients");
    }
    let host_name = args.name.clone().unwrap_or_else(hostname);

    let rt = tokio::runtime::Runtime::new()?;
    // quinn needs to be inside the runtime when the endpoint is created.
    let endpoint = rt.block_on(async { make_endpoint(&store, args.port) })?;
    let overrides = Overrides { max_fps: args.max_fps, codec: args.codec, no_input: args.no_input };
    let bitrate = args.bitrate_mbps.map(|m| (m * 1e6) as u32);
    let ctx = Arc::new(Ctx::new(store, host_name, endpoint, overrides, bitrate, args.encoder, args.test_pattern)?);
    if !args.no_mdns {
        *ctx.advertisement.lock().unwrap() = Some(advertise(&ctx.store.host_id, &ctx.host_name, ctx.port)?);
    }

    tracing::info!("freemonplay host \"{}\" listening on UDP port {}", ctx.host_name, ctx.port);
    if !gui {
        println!();
        println!("  freemonplay host \"{}\" listening on UDP port {}", ctx.host_name, ctx.port);
        println!("  Pairing code for new clients:  {}", ctx.store.pairing_code());
        println!("  Host fingerprint: {}", &ctx.store.cert_fingerprint()[..16]);
        println!();
    }
    rt.spawn(accept_loop(ctx.clone()));

    if gui {
        #[cfg(target_os = "macos")]
        menubar::run(ctx.clone(), log_path());
    } else {
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
        });
    }

    tracing::info!("shutting down");
    ctx.shutdown();
    // Let sessions see the close and drop their virtual displays.
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

async fn accept_loop(ctx: Arc<Ctx>) {
    while let Some(incoming) = ctx.endpoint.accept().await {
        tracing::info!("{}: connecting", incoming.remote_address());
        tokio::spawn(session::run(incoming, ctx.clone()));
    }
}

pub(crate) fn launched_from_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

fn log_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Logs/freemonplay/host.log")
}

/// Terminal mode logs to stderr; the menu bar app (which has no terminal) to a file.
fn init_logging(to_file: bool) -> Result<()> {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,quinn=warn,mdns_sd=warn".into())
    };
    if to_file {
        let path = log_path();
        std::fs::create_dir_all(path.parent().context("log path has no parent")?)?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        tracing_subscriber::fmt().with_env_filter(filter()).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter()).init();
    }
    Ok(())
}

fn make_endpoint(store: &HostStore, port: u16) -> Result<quinn::Endpoint> {
    let cert = rustls::pki_types::CertificateDer::from(store.cert_der.clone());
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(store.key_der.clone());
    let mut tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert], key.into())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        .send_window(16 * 1024 * 1024);
    config.transport_config(Arc::new(transport));

    // Dual-stack socket: accepts IPv4 and IPv6.
    let addr = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
    quinn::Endpoint::server(config, addr).with_context(|| format!("can't listen on UDP port {port}"))
}

/// Our mDNS announcement. Withdrawing it tells clients right away that we've gone, rather
/// than leaving a stale entry in their host lists until it expires.
pub struct Advertisement {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    pub fn withdraw(self) {
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}

fn advertise(host_id: &str, name: &str, port: u16) -> Result<Advertisement> {
    let mdns = mdns_sd::ServiceDaemon::new()?;
    // Only advertise LAN IPv4 addresses. Loopback is useless to other machines, and IPv6
    // link-local addresses (e.g. macOS's fe80::1 on lo0) can't be dialled without a scope id.
    mdns.disable_interface(vec![mdns_sd::IfKind::IPv6, mdns_sd::IfKind::LoopbackV4])?;
    let host = format!("freemonplay-{host_id}.local.");
    let version = PROTOCOL_VERSION.to_string();
    let props = [("id", host_id), ("name", name), ("proto", version.as_str())];
    let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, name, &host, "", port, &props[..])?.enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    mdns.register(info)?;
    Ok(Advertisement { daemon: mdns, fullname })
}

fn probe_display(size: &str) -> Result<()> {
    let (size, refresh_hz) = match size.split_once(':') {
        Some((s, hz)) => (s, hz.parse().context("refresh rate after ':' must be a number")?),
        None => (size, 60),
    };
    let (size, hidpi) = match size.strip_suffix("@2x") {
        Some(s) => (s, true),
        None => (size, false),
    };
    let (w, h) = size.split_once('x').context("expected WxH, e.g. 1920x1080, 2732x2048@2x or 1920x1080:30")?;
    let req = fmp_proto::DisplayRequest {
        width: w.parse()?,
        height: h.parse()?,
        refresh_hz,
        hidpi,
        size_mm: None,
    };
    let display = platform::VirtualDisplay::create("freemonplay probe", 1, &req)?;
    println!("virtual display {} is up; removing it in 3 s", display.id());
    std::thread::sleep(Duration::from_secs(3));
    Ok(())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "freemonplay host".into())
}

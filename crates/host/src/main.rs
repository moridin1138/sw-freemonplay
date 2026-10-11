//! freemonplay host: advertises itself on the LAN, and for every client that connects it
//! creates a virtual display matching the client's screen and streams it there.

mod encoder;
mod frame;
mod pattern;
mod platform;
mod session;
mod store;

use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use fmp_proto::{ALPN, DEFAULT_PORT, PROTOCOL_VERSION, SERVICE_TYPE};

use session::Ctx;
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
    /// Frame rate cap. Each client also caps it at its own refresh rate.
    #[arg(long, default_value_t = 60)]
    max_fps: u32,
    /// Video bitrate in Mbit/s. Defaults to a value based on resolution and frame rate.
    #[arg(long)]
    bitrate_mbps: Option<f64>,
    /// Video encoder.
    #[arg(long, value_enum, default_value_t = encoder::EncoderChoice::Auto)]
    encoder: encoder::EncoderChoice,
    /// Video codec. Auto uses the client's preference (HEVC when it can decode it in
    /// hardware) as long as this host can encode it.
    #[arg(long, value_enum, default_value_t = encoder::CodecChoice::Auto)]
    codec: encoder::CodecChoice,
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
    /// Diagnostic: create a virtual display for a few seconds. Examples: 1920x1080,
    /// 2732x2048@2x (HiDPI), 1920x1080:30 (30 Hz).
    #[arg(long, value_name = "WxH")]
    probe_display: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,quinn=warn,mdns_sd=warn".into()),
        )
        .init();
    let args = Args::parse();
    let _ = rustls::crypto::ring::default_provider().install_default();

    if let Some(size) = args.probe_display {
        return probe_display(&size);
    }
    if args.test_pattern {
        tracing::warn!("test pattern mode: clients get a generated picture, not a virtual display");
    } else {
        platform::check_permissions()?;
    }

    let store = HostStore::open(args.config_dir)?;
    if args.forget_clients {
        store.forget_all()?;
        tracing::info!("forgot all paired clients");
    }
    let host_name = args.name.unwrap_or_else(hostname);

    let endpoint = make_endpoint(&store, args.port)?;
    let port = endpoint.local_addr()?.port();
    let _mdns = if args.no_mdns { None } else { Some(advertise(&store.host_id, &host_name, port)?) };

    println!();
    println!("  freemonplay host \"{host_name}\" listening on UDP port {port}");
    println!("  Pairing code for new clients:  {}", store.pairing_code());
    println!("  Host fingerprint: {}", &store.cert_fingerprint()[..16]);
    println!();

    let ctx = Arc::new(Ctx {
        store,
        host_name,
        max_fps: args.max_fps.clamp(1, 240),
        bitrate_bps: args.bitrate_mbps.map(|m| (m * 1e6) as u32),
        encoder: args.encoder,
        codec: args.codec.only(),
        allow_input: !args.no_input,
        test_pattern: args.test_pattern,
    });

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                tracing::info!("{}: connecting", incoming.remote_address());
                tokio::spawn(session::run(incoming, ctx.clone()));
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    tracing::info!("shutting down");
    endpoint.close(0u32.into(), b"host shutting down");
    // Let sessions see the close and drop their virtual displays.
    tokio::time::sleep(Duration::from_millis(300)).await;
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

fn advertise(host_id: &str, name: &str, port: u16) -> Result<mdns_sd::ServiceDaemon> {
    let mdns = mdns_sd::ServiceDaemon::new()?;
    // Only advertise LAN IPv4 addresses. Loopback is useless to other machines, and IPv6
    // link-local addresses (e.g. macOS's fe80::1 on lo0) can't be dialled without a scope id.
    mdns.disable_interface(vec![mdns_sd::IfKind::IPv6, mdns_sd::IfKind::LoopbackV4])?;
    let host = format!("freemonplay-{host_id}.local.");
    let version = PROTOCOL_VERSION.to_string();
    let props = [("id", host_id), ("name", name), ("proto", version.as_str())];
    let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, name, &host, "", port, &props[..])?.enable_addr_auto();
    mdns.register(info)?;
    Ok(mdns)
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

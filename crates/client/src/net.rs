//! Finding hosts, connecting, and running the session once the window exists.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::{
    ALPN, Auth, ClientMsg, Codec, DEFAULT_PORT, DisplayRequest, Hello, HostMsg, PROTOCOL_VERSION, RejectKind,
    SERVICE_TYPE, ScreenId, VideoFrame, pairing,
    read_msg, write_msg,
};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use winit::event_loop::EventLoopProxy;

use crate::decode::{self, FrameSlot};
use crate::store::{self, ClientStore, hex};
use crate::UserEvent;

#[derive(Clone)]
pub struct Target {
    pub name: String,
    /// Addresses to try, best first.
    pub addrs: Vec<SocketAddr>,
    /// The host's advertised id (discovered hosts only).
    pub id: Option<String>,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let addrs: Vec<String> = self.addrs.iter().map(SocketAddr::to_string).collect();
        write!(f, "{} ({})", self.name, addrs.join(", "))
    }
}

/// Hosts currently advertising on the LAN, kept up to date in the background.
pub type HostList = Arc<Mutex<Vec<Target>>>;

/// Browses mDNS continuously; `changed` is called whenever the list changes.
pub fn browse(changed: impl Fn() + Send + 'static) -> Result<HostList> {
    let mdns = mdns_sd::ServiceDaemon::new()?;
    let rx = mdns.browse(SERVICE_TYPE)?;
    let list: HostList = Arc::default();
    let shared = list.clone();
    std::thread::Builder::new().name("discovery".into()).spawn(move || {
        let _mdns = mdns; // keep the daemon alive with the thread
        // A host's addresses can arrive over several answers, so merge them per service.
        let mut found: BTreeMap<String, (Option<String>, String, u16, Vec<IpAddr>)> = BTreeMap::new();
        while let Ok(event) = rx.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let id = info.get_property_val_str("id").map(str::to_string);
                    let name = info.get_property_val_str("name").unwrap_or(&info.fullname).to_string();
                    let entry = found.entry(info.fullname.clone()).or_insert_with(|| (id, name, info.port, Vec::new()));
                    entry.2 = info.port;
                    for ip in info.addresses.iter().map(|a| a.to_ip_addr()) {
                        if !entry.3.contains(&ip) {
                            entry.3.push(ip);
                        }
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    found.remove(&fullname);
                }
                _ => continue,
            }
            let mut targets: Vec<Target> = found
                .values()
                .filter_map(|(id, name, port, ips)| {
                    let addrs = rank_addresses(ips.clone(), *port);
                    (!addrs.is_empty()).then(|| Target { name: name.clone(), addrs, id: id.clone() })
                })
                .collect();
            targets.sort_by_key(|a| a.name.to_lowercase());
            *shared.lock().unwrap() = targets;
            changed();
        }
    })?;
    Ok(list)
}

/// Drops addresses another machine can't use (loopback, IPv6 link-local without a scope
/// id) and orders the rest: private IPv4 first, then other IPv4, then IPv6.
fn rank_addresses(ips: Vec<IpAddr>, port: u16) -> Vec<SocketAddr> {
    let mut ips: Vec<IpAddr> = ips
        .into_iter()
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_unspecified(),
            IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && !v6.is_unicast_link_local(),
        })
        .collect();
    ips.sort_by_key(|ip| match ip {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(v4) if v4.is_link_local() => 2,
        IpAddr::V4(_) => 1,
        IpAddr::V6(_) => 3,
    });
    ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()
}

/// Parses `host`, `host:port`, `1.2.3.4`, `[::1]:47800`, ...
pub async fn resolve(spec: &str) -> Result<Target> {
    let with_port = if spec.parse::<IpAddr>().is_ok() || !spec.contains(':') {
        let host = if spec.contains(':') { format!("[{spec}]") } else { spec.to_string() };
        format!("{host}:{DEFAULT_PORT}")
    } else {
        spec.to_string()
    };
    let mut addrs: Vec<SocketAddr> =
        tokio::net::lookup_host(&with_port).await.with_context(|| format!("can't resolve {spec}"))?.collect();
    // Prefer IPv4, matching what discovery does.
    addrs.sort_by_key(|a| a.is_ipv6());
    if addrs.is_empty() {
        bail!("no address for {spec}");
    }
    Ok(Target { name: spec.to_string(), addrs, id: None })
}

/// Tries each of the target's addresses in turn and returns the first connection.
pub async fn connect_any(target: &Target) -> Result<Connected> {
    let mut last_error = None;
    for &addr in &target.addrs {
        match connect(addr).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                tracing::debug!("{addr}: {e:#}");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no addresses to try")))
}

pub struct Connected {
    /// Not used directly, but dropping it would tear down the connection.
    #[allow(dead_code)]
    pub endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    /// SHA-256 of the host's certificate, hex.
    pub fingerprint: String,
}

pub async fn connect(addr: SocketAddr) -> Result<Connected> {
    let seen = Arc::new(Mutex::new(None));
    let verifier = Arc::new(RecordingVerifier {
        seen: seen.clone(),
        provider: Arc::new(rustls::crypto::ring::default_provider()),
    });
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into()?))
        // A 4K keyframe can be a couple of MB; don't make it wait on flow control.
        .stream_receive_window((16u32 * 1024 * 1024).into())
        .receive_window((32u32 * 1024 * 1024).into());
    config.transport_config(Arc::new(transport));

    let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(config);
    let conn = tokio::time::timeout(Duration::from_secs(3), endpoint.connect(addr, "freemonplay")?)
        .await
        .with_context(|| format!("no answer from {addr} (is the host running? firewall?)"))??;
    let fingerprint = seen.lock().unwrap().clone().context("host sent no certificate")?;
    Ok(Connected { endpoint, conn, fingerprint })
}

/// We don't use certificate authorities: the host's certificate is self-signed, and we
/// trust it by pinning its fingerprint after pairing. Signatures are still checked, so
/// only the holder of that certificate's key can complete the handshake.
#[derive(Debug)]
struct RecordingVerifier {
    seen: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap() = Some(hex(&Sha256::digest(end_entity)));
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// How we prove ourselves to the host.
pub enum Credentials {
    /// Issued when we paired before.
    Token(Vec<u8>),
    /// First time: the code the host shows. Used for SPAKE2 pairing, never sent.
    Code(String),
}

/// Result of connecting to a host before starting a session.
pub enum Prepared {
    /// Connected, with a saved pairing (or a code to pair with).
    Ready(Connected, Credentials),
    /// First time with this host: ask for its code, then call `prepare_with_code`.
    NeedCode { fingerprint: String },
}

/// Connects and checks whether we're already paired. Doesn't hold the connection open
/// while a person types a code: the host only waits a few seconds for a session to start.
pub async fn prepare(target: &Target, store: &Mutex<ClientStore>, code: Option<String>) -> Result<Prepared> {
    let connected = connect_any(target).await?;
    let token = store.lock().unwrap().host(&connected.fingerprint).and_then(|h| store::unhex(&h.token_hex));
    if let Some(token) = token {
        return Ok(Prepared::Ready(connected, Credentials::Token(token)));
    }
    match code {
        Some(code) => Ok(Prepared::Ready(connected, Credentials::Code(code))),
        None => {
            connected.conn.close(0u32.into(), b"pairing");
            Ok(Prepared::NeedCode { fingerprint: connected.fingerprint })
        }
    }
}

/// Reconnects to pair with `code`, checking it's still the host we asked the code for.
pub async fn prepare_with_code(target: &Target, code: String, fingerprint: &str) -> Result<Prepared> {
    let connected = connect_any(target).await?;
    if connected.fingerprint != fingerprint {
        bail!("the host's identity changed while pairing; not continuing");
    }
    Ok(Prepared::Ready(connected, Credentials::Code(code)))
}

pub struct Session {
    /// Which of the app's displays (windows) this session feeds; tags its events.
    pub view: usize,
    /// Which monitor, when the client uses several (`None`: the main one).
    pub screen: Option<ScreenId>,
    pub connected: Connected,
    pub credentials: Credentials,
    pub client_name: String,
    /// Codecs to offer, most preferred first.
    pub codecs: Vec<Codec>,
    pub store: Arc<Mutex<ClientStore>>,
    pub display: DisplayRequest,
    /// The renderer's GPU (Windows: adapter LUID), for zero-copy decoding.
    pub gpu_adapter: Option<u64>,
    pub outgoing: mpsc::UnboundedReceiver<ClientMsg>,
    pub outgoing_tx: mpsc::UnboundedSender<ClientMsg>,
    pub frames: Arc<FrameSlot>,
    pub proxy: EventLoopProxy<UserEvent>,
}

/// Runs until the connection ends. Returns a human readable reason.
pub async fn run(mut s: Session) -> Result<String> {
    let conn = s.connected.conn.clone();
    let (mut send, mut recv) = conn.open_bi().await?;
    let display = s.display;

    let (auth, mut pairing) = match &s.credentials {
        Credentials::Token(token) => (Auth::Token(token.clone()), None),
        Credentials::Code(code) => {
            let (state, msg) = pairing::Pairing::client(code);
            (Auth::Pake(msg), Some(state))
        }
    };
    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        client_id: s.store.lock().unwrap().client_id().to_string(),
        client_name: s.client_name.clone(),
        auth,
        display,
        codecs: s.codecs.clone(),
        screen: s.screen.clone(),
    };
    write_msg(&mut send, &ClientMsg::Hello(hello)).await?;

    let mut reply = read_msg::<HostMsg>(&mut recv).await?;
    if let Some(HostMsg::PairingChallenge { pake, proof }) = &reply {
        let state = pairing.take().context("host started pairing, but we didn't ask to pair")?;
        let key = state.finish(pake)?;
        let binding = pairing::channel_binding(&conn)?;
        if !key.verify(pairing::Role::Host, &binding, proof) {
            conn.close(0u32.into(), b"pairing failed");
            bail!(
                "pairing failed: wrong code, or something is interfering with the connection. \
                 Check the code shown on the host and try again"
            );
        }
        write_msg(&mut send, &ClientMsg::PairingProof(key.confirmation(pairing::Role::Client, &binding))).await?;
        reply = read_msg::<HostMsg>(&mut recv).await?;
    }

    let welcome = match reply {
        Some(HostMsg::Welcome(w)) => w,
        Some(HostMsg::Rejected { reason, kind }) => {
            // Only drop our pairing when the host says it no longer knows us; a codec or
            // version problem doesn't mean we should have to pair again.
            if kind == RejectKind::NotPaired {
                let _ = s.store.lock().unwrap().forget(&s.connected.fingerprint);
            }
            bail!("host refused: {reason}");
        }
        Some(HostMsg::Notice { message }) => bail!("unexpected notice before welcome: {message}"),
        Some(HostMsg::PairingChallenge { .. }) => bail!("host asked to pair twice"),
        None => bail!("host closed the connection during the handshake"),
    };
    if let Some(token) = &welcome.issued_token {
        s.store.lock().unwrap().remember(&s.connected.fingerprint, &welcome.host_name, &welcome.host_id, token)?;
        tracing::info!("paired with {}; no code needed next time", welcome.host_name);
    }
    let _ = s.proxy.send_event(UserEvent::Connected {
        view: s.view,
        host_name: welcome.host_name.clone(),
        width: welcome.width,
        height: welcome.height,
        codec: welcome.codec,
    });

    let mut video = conn.accept_uni().await?;
    // Small queue: if decoding can't keep up we'd rather skip ahead to a keyframe than lag.
    let (frame_tx, frame_rx) = std_mpsc::sync_channel::<(VideoFrame, Instant)>(4);
    let (ready_tx, ready_rx) = oneshot::channel();
    {
        let (frames, proxy, tx) = (s.frames.clone(), s.proxy.clone(), s.outgoing_tx.clone());
        let (codec, gpu_adapter, view) = (welcome.codec, s.gpu_adapter, s.view);
        std::thread::Builder::new()
            .name("decoder".into())
            .spawn(move || decode::run(view, codec, frame_rx, frames, proxy, &tx, ready_tx, gpu_adapter))?;
    }
    // Let video wait in the network buffer until the decoder can take it, rather than
    // discarding frames (and asking for keyframes) while it starts up. If it failed to
    // start, it has already told the UI why.
    if ready_rx.await.is_err() {
        return Ok("decoder failed to start".into());
    }

    let reader = async {
        let mut waiting_for_keyframe = false;
        while let Some(frame) = read_msg::<VideoFrame>(&mut video).await? {
            if waiting_for_keyframe && !frame.keyframe {
                continue;
            }
            waiting_for_keyframe = false;
            match frame_tx.try_send((frame, Instant::now())) {
                Ok(()) => {}
                Err(std_mpsc::TrySendError::Full(_)) => {
                    tracing::debug!("decoder behind, skipping to next keyframe");
                    waiting_for_keyframe = true;
                    let _ = s.outgoing_tx.send(ClientMsg::RequestKeyframe);
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
        anyhow::Ok(())
    };
    let writer = async {
        while let Some(mut msg) = s.outgoing.recv().await {
            // The UI measures display latency; the network side knows the round trip.
            if let ClientMsg::Stats(stats) = &mut msg {
                stats.rtt_ms = (conn.rtt().as_secs_f64() * 1e3) as f32;
            }
            write_msg(&mut send, &msg).await?;
        }
        anyhow::Ok(())
    };
    let notices = async {
        while let Some(msg) = read_msg::<HostMsg>(&mut recv).await? {
            if let HostMsg::Notice { message } = msg {
                tracing::warn!("host says: {message}");
                eprintln!("Host says: {message}");
            }
        }
        // The host finished its side of the control stream; video may still be flowing.
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };

    tokio::select! {
        r = reader => r.map(|()| "host stopped streaming".to_string()),
        r = writer => r.map(|()| "closed".to_string()),
        r = notices => r.map(|()| "closed".to_string()),
        e = conn.closed() => Ok(match e {
            quinn::ConnectionError::ApplicationClosed(c) => String::from_utf8_lossy(&c.reason).into_owned(),
            other => other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_usable_addresses() {
        let ips: Vec<IpAddr> =
            ["fe80::1", "::1", "127.0.0.1", "2001:db8::5", "169.254.3.4", "192.168.0.25", "100.64.1.2"]
                .iter()
                .map(|s| s.parse().unwrap())
                .collect();
        let ranked: Vec<String> = rank_addresses(ips, 47800).iter().map(SocketAddr::to_string).collect();
        assert_eq!(ranked, ["192.168.0.25:47800", "100.64.1.2:47800", "169.254.3.4:47800", "[2001:db8::5]:47800"]);
    }
}

//! Finding hosts, connecting, and running the session once the window exists.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::{
    ALPN, Auth, ClientMsg, Codec, DEFAULT_PORT, DisplayRequest, Hello, HostMsg, PROTOCOL_VERSION, RejectKind,
    SERVICE_TYPE, VideoFrame,
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
use crate::store::{ClientStore, hex};
use crate::UserEvent;

pub struct Target {
    pub name: String,
    /// Addresses to try, best first.
    pub addrs: Vec<SocketAddr>,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let addrs: Vec<String> = self.addrs.iter().map(SocketAddr::to_string).collect();
        write!(f, "{} ({})", self.name, addrs.join(", "))
    }
}

/// Browses mDNS for `wait` and returns every host that answered.
pub fn discover(wait: Duration) -> Result<Vec<Target>> {
    let mdns = mdns_sd::ServiceDaemon::new()?;
    let rx = mdns.browse(SERVICE_TYPE)?;
    let deadline = Instant::now() + wait;
    // A host's addresses can arrive over several answers, so merge them per host id.
    let mut found: BTreeMap<String, (String, u16, Vec<IpAddr>)> = BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else { break };
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            let id = info.get_property_val_str("id").unwrap_or(&info.fullname).to_string();
            let name = info.get_property_val_str("name").unwrap_or(&info.fullname).to_string();
            let entry = found.entry(id).or_insert_with(|| (name, info.port, Vec::new()));
            for ip in info.addresses.iter().map(|a| a.to_ip_addr()) {
                if !entry.2.contains(&ip) {
                    entry.2.push(ip);
                }
            }
        }
    }
    let _ = mdns.shutdown();
    Ok(found
        .into_values()
        .filter_map(|(name, port, ips)| {
            let addrs = rank_addresses(ips, port);
            (!addrs.is_empty()).then_some(Target { name, addrs })
        })
        .collect())
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
    Ok(Target { name: spec.to_string(), addrs })
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

pub struct Session {
    pub connected: Connected,
    pub auth: Auth,
    pub client_name: String,
    /// Codecs to offer, most preferred first.
    pub codecs: Vec<Codec>,
    pub store: ClientStore,
    pub display: oneshot::Receiver<DisplayRequest>,
    pub outgoing: mpsc::UnboundedReceiver<ClientMsg>,
    pub outgoing_tx: mpsc::UnboundedSender<ClientMsg>,
    pub frames: Arc<FrameSlot>,
    pub proxy: EventLoopProxy<UserEvent>,
}

/// Runs until the connection ends. Returns a human readable reason.
pub async fn run(mut s: Session) -> Result<String> {
    let conn = s.connected.conn.clone();
    let (mut send, mut recv) = conn.open_bi().await?;
    let display = s.display.await.context("window never opened")?;

    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        client_id: s.store.client_id().to_string(),
        client_name: s.client_name.clone(),
        auth: s.auth.clone(),
        display,
        codecs: s.codecs.clone(),
    };
    write_msg(&mut send, &ClientMsg::Hello(hello)).await?;

    let welcome = match read_msg::<HostMsg>(&mut recv).await? {
        Some(HostMsg::Welcome(w)) => w,
        Some(HostMsg::Rejected { reason, kind }) => {
            // Only drop our pairing when the host says it no longer knows us; a codec or
            // version problem doesn't mean we should have to pair again.
            if kind == RejectKind::NotPaired {
                let _ = s.store.forget(&s.connected.fingerprint);
            }
            bail!("host refused: {reason}");
        }
        Some(HostMsg::Notice { message }) => bail!("unexpected notice before welcome: {message}"),
        None => bail!("host closed the connection during the handshake"),
    };
    if let Some(token) = &welcome.issued_token {
        s.store.remember(&s.connected.fingerprint, &welcome.host_name, token)?;
        tracing::info!("paired with {}; no code needed next time", welcome.host_name);
    }
    let _ = s.proxy.send_event(UserEvent::Connected {
        host_name: welcome.host_name.clone(),
        width: welcome.width,
        height: welcome.height,
        codec: welcome.codec,
    });

    let mut video = conn.accept_uni().await?;
    // Small queue: if decoding can't keep up we'd rather skip ahead to a keyframe than lag.
    let (frame_tx, frame_rx) = std_mpsc::sync_channel::<VideoFrame>(4);
    {
        let (frames, proxy, tx) = (s.frames.clone(), s.proxy.clone(), s.outgoing_tx.clone());
        let codec = welcome.codec;
        std::thread::Builder::new()
            .name("decoder".into())
            .spawn(move || decode::run(codec, frame_rx, frames, proxy, &tx))?;
    }

    let reader = async {
        let mut waiting_for_keyframe = false;
        while let Some(frame) = read_msg::<VideoFrame>(&mut video).await? {
            if waiting_for_keyframe && !frame.keyframe {
                continue;
            }
            waiting_for_keyframe = false;
            match frame_tx.try_send(frame) {
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
        while let Some(msg) = s.outgoing.recv().await {
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

//! Wire protocol shared by the freemonplay host and client.
//!
//! One QUIC connection per screen:
//! - The client opens a bidirectional **control stream**. It sends [`ClientMsg::Hello`]
//!   and gets back [`HostMsg::Welcome`] or [`HostMsg::Rejected`]. Input events and
//!   keyframe requests then flow client to host on the same stream.
//! - After a welcome the host opens one unidirectional **video stream** carrying
//!   [`VideoFrame`]s (H.264 Annex-B access units) in order.
//!
//! Every message is a little-endian `u32` length followed by a postcard body.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod hid;

pub const PROTOCOL_VERSION: u32 = 1;
pub const ALPN: &[u8] = b"freemonplay/1";
pub const SERVICE_TYPE: &str = "_freemonplay._udp.local.";
pub const DEFAULT_PORT: u16 = 47800;

/// Upper bound on a single framed message, so a bad peer can't make us allocate gigabytes.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello(Hello),
    Input(InputEvent),
    /// The decoder lost sync (or just started) and needs an IDR frame.
    RequestKeyframe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// Stable random id the client generates once and keeps.
    pub client_id: String,
    /// Human readable, e.g. the machine's hostname. Also used as the virtual display name.
    pub client_name: String,
    pub auth: Auth,
    pub display: DisplayRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Auth {
    /// First connection: the 6 digit code the host prints at startup.
    PairingCode(String),
    /// Later connections: the token the host issued when we paired.
    Token(Vec<u8>),
}

/// What the client's screen looks like, so the host can create a matching virtual display.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DisplayRequest {
    /// Physical pixels.
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    /// Render at 2x (Retina style) instead of 1:1 pixels.
    pub hidpi: bool,
    /// Physical size, if known. Lets the host OS pick sensible text sizes.
    pub size_mm: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostMsg {
    Welcome(Welcome),
    Rejected { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub host_id: String,
    pub host_name: String,
    /// Resolution of the stream the host will actually send.
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    /// Present only right after pairing with a code. Store it and send it next time.
    pub issued_token: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    /// Capture time in microseconds since the stream started.
    pub pts_us: u64,
    pub keyframe: bool,
    /// H.264 Annex-B access unit.
    pub data: Vec<u8>,
}

/// Pointer coordinates are normalised to 0.0..=1.0 across the virtual display, so the
/// client doesn't need to know how the host scales it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    PointerMove { x: f32, y: f32 },
    PointerButton { button: MouseButton, pressed: bool },
    /// `precise` means pixel deltas (trackpad); otherwise whole lines (mouse wheel).
    Scroll { dx: f32, dy: f32, precise: bool },
    /// `usage` is a USB HID keyboard usage id, see [`hid`].
    Key { usage: u16, pressed: bool },
    Touch { id: u64, phase: TouchPhase, x: f32, y: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let body = postcard::to_stdvec(msg)?;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub async fn write_msg<T: Serialize>(stream: &mut quinn::SendStream, msg: &T) -> Result<()> {
    stream.write_all(&encode(msg)?).await?;
    Ok(())
}

/// Reads one message. Returns `Ok(None)` when the peer cleanly finished the stream.
pub async fn read_msg<T: DeserializeOwned>(stream: &mut quinn::RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE_BYTES {
        bail!("message of {len} bytes exceeds limit");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.context("truncated message")?;
    Ok(Some(postcard::from_bytes(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_input() {
        let msg = ClientMsg::Input(InputEvent::Touch { id: 3, phase: TouchPhase::Moved, x: 0.25, y: 0.75 });
        let bytes = encode(&msg).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize, bytes.len() - 4);
        let back: ClientMsg = postcard::from_bytes(&bytes[4..]).unwrap();
        assert!(matches!(back, ClientMsg::Input(InputEvent::Touch { id: 3, .. })));
    }
}

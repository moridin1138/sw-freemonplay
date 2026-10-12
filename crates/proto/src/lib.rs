//! Wire protocol shared by the freemonplay host and client.
//!
//! One QUIC connection per screen:
//! - The client opens a bidirectional **control stream**. It sends [`ClientMsg::Hello`]
//!   and gets back [`HostMsg::Welcome`] or [`HostMsg::Rejected`]. Input events and
//!   keyframe requests then flow client to host on the same stream.
//! - After a welcome the host opens one unidirectional **video stream** carrying
//!   [`VideoFrame`]s (Annex-B access units in the negotiated [`Codec`]) in order.
//!
//! Every message is a little-endian `u32` length followed by a postcard body.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod hid;
pub mod pairing;

pub const PROTOCOL_VERSION: u32 = 8;
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
    /// Sent every couple of seconds so the host can show latency and frame rate.
    Stats(ClientStats),
    /// Pairing step 3: the client's proof (see [`pairing`]).
    PairingProof(Vec<u8>),
    /// The client's network link, once known, for the host to show.
    Network(NetworkInfo),
    /// Keyboard and mouse mode: the pointer left the client's monitors back toward the
    /// host, which takes its keyboard and mouse back.
    ReturnControl(ReturnAt),
    /// The client's clipboard text, sent as control returns to the host when it changed.
    Clipboard(String),
}

/// Where the pointer goes back to the host.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ReturnAt {
    /// Mixed layout: it moved onto the monitor showing the host's screen, at this point
    /// (0 to 1 across and down that monitor).
    Screen { x: f32, y: f32 },
    /// Only keyboard and mouse monitors: it crossed the edge leading back to the host
    /// (the `back` edge of [`HostMsg::RemoteEnter`]), this far along it (0 to 1).
    Edge { along: f32 },
}

/// The client's monitors in its own desktop coordinates (physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64 && x < self.right() as f64 && y >= self.y as f64 && y < self.bottom() as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

/// Keyboard and mouse mode: which of the client's monitors the host's keyboard and mouse
/// can move onto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvmLayout {
    /// Monitors the host's pointer can move onto (the client's own desktop).
    pub monitors: Vec<Rect>,
    /// Mixed layout: the monitor showing this session's screen. The pointer reaches
    /// `monitors` by leaving the host's screen toward them. `None`: no screen is streamed,
    /// and the monitors sit beside one of the host's own displays (the host decides which
    /// side).
    pub screen: Option<Rect>,
}

/// The host's keyboard and mouse, while they control the client.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RemoteInput {
    /// Relative movement in the host's points (its pointer acceleration already applied).
    Move { dx: f32, dy: f32 },
    Button { button: MouseButton, pressed: bool },
    /// Positive `dy` scrolls up; `precise` means pixels (trackpad), otherwise lines.
    Scroll { dx: f32, dy: f32, precise: bool },
    /// `usage` is a USB HID keyboard usage id, see [`hid`].
    Key { usage: u16, pressed: bool },
}

/// How the client reaches the host, described for people (e.g. "Wi-Fi 6, 5 GHz or higher,
/// signal 80%, link 866 Mbit/s"), with tips when the link is likely to limit streaming.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub summary: String,
    pub advice: Vec<String>,
}

/// What the client measured over the last reporting window, on its own clock.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ClientStats {
    /// Pictures actually shown per second.
    pub shown_fps: f32,
    /// Average time from a frame fully arriving to it being presented (decode, upload,
    /// present), in milliseconds. Excludes waiting for the display's next refresh.
    pub display_ms: f32,
    /// The client's view of the network round-trip time, in milliseconds.
    pub rtt_ms: f32,
    /// Frames that took more than 30 ms longer from capture to arrival than the quickest
    /// frame in the same window: delayed in transit (lost packets resent, Wi-Fi stalls).
    pub late_frames: u32,
    /// The most delayed frame's extra time over the quickest, in milliseconds.
    pub worst_late_ms: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    /// Must stay the first field so a peer on a different version can still read it
    /// (see [`peek_hello_protocol`]).
    pub protocol: u32,
    /// Stable random id the client generates once and keeps.
    pub client_id: String,
    /// Human readable, e.g. the machine's hostname. Also used as the virtual display name.
    pub client_name: String,
    pub auth: Auth,
    pub display: DisplayRequest,
    /// Codecs the client can decode, most preferred first.
    pub codecs: Vec<Codec>,
    /// Which of the client's monitors this session fills, when it uses several. `None` is
    /// its main one (the monitor its window is on), which keeps the client's original
    /// identity on the host.
    pub screen: Option<ScreenId>,
    /// Keyboard and mouse mode: monitors the host's keyboard and mouse may move onto.
    pub kvm: Option<KvmLayout>,
    /// No screen: only keyboard and mouse (and the clipboard). The host creates no display.
    pub input_only: bool,
}

/// One of a client's monitors, so the host gives each its own virtual display that the
/// host OS remembers (arrangement, resolution).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenId {
    /// Stable for that monitor on the client, e.g. its OS name.
    pub key: String,
    /// Shown to the user, e.g. "DISPLAY2".
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    /// HEVC. Roughly half the bitrate of H.264 for the same quality.
    H265,
}

impl std::fmt::Display for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Codec::H264 => "H.264",
            Codec::H265 => "HEVC",
        })
    }
}

impl Codec {
    /// NAL unit type from the first byte(s) of a NAL unit (after the start code).
    pub fn nal_type(self, nal: &[u8]) -> u8 {
        match self {
            Codec::H264 => nal.first().map_or(0, |b| b & 0x1F),
            Codec::H265 => nal.first().map_or(0, |b| (b >> 1) & 0x3F),
        }
    }

    /// Parameter set NAL types, in the order decoders want them (VPS, SPS, PPS).
    pub fn parameter_set_types(self) -> &'static [u8] {
        match self {
            Codec::H264 => &[7, 8],
            Codec::H265 => &[32, 33, 34],
        }
    }

    /// Whether a NAL type starts a picture the decoder can begin from.
    pub fn is_keyframe_nal(self, nal_type: u8) -> bool {
        match self {
            Codec::H264 => nal_type == 5,
            // BLA, IDR and CRA pictures (IRAP).
            Codec::H265 => (16..=21).contains(&nal_type),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Auth {
    /// First connection: pairing step 1, a SPAKE2 message derived from the host's 6 digit
    /// code (the code itself is never sent; see [`pairing`]).
    Pake(Vec<u8>),
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
    /// `kind` comes after `reason` so clients on other protocol versions can still read
    /// the reason (postcard ignores trailing bytes).
    Rejected { reason: String, kind: RejectKind },
    /// Something the user should know about after the session started (e.g. no frames
    /// are being captured). Informational; the session continues.
    Notice { message: String },
    /// Pairing step 2: the host's SPAKE2 message and its proof (see [`pairing`]).
    PairingChallenge { pake: Vec<u8>, proof: Vec<u8> },
    /// Keyboard and mouse mode: the host's pointer moved onto the client's `monitor` (an
    /// index into [`KvmLayout::monitors`]) at this point (0 to 1 across and down it). The
    /// client now gets [`HostMsg::Remote`] input until it returns control. `back` is the
    /// edge of the client's monitors leading back to the host (`None` in a mixed layout,
    /// where moving onto the host's screen returns).
    RemoteEnter { monitor: u32, x: f32, y: f32, back: Option<Edge> },
    Remote(RemoteInput),
    /// The host took its keyboard and mouse back (its escape shortcut, or it gave up).
    RemoteLeave,
    /// The host's clipboard text, sent as control moves to the client when it changed.
    Clipboard(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectKind {
    /// The host doesn't recognise the client's token; the client should forget it and pair again.
    NotPaired,
    WrongCode,
    VersionMismatch,
    NoCommonCodec,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub host_id: String,
    pub host_name: String,
    /// Resolution of the stream the host will actually send.
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub codec: Codec,
    /// Present only right after pairing with a code. Store it and send it next time.
    pub issued_token: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    /// Capture time in microseconds since the stream started.
    pub pts_us: u64,
    pub keyframe: bool,
    /// Annex-B access unit in the negotiated codec. Keyframes carry their parameter sets.
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

/// Reads just the protocol version from a serialized [`ClientMsg::Hello`], even when the
/// rest of the message is from a different protocol version and won't deserialize.
pub fn peek_hello_protocol(body: &[u8]) -> Option<u32> {
    #[derive(Deserialize)]
    enum Peek {
        Hello(u32),
    }
    match postcard::take_from_bytes::<Peek>(body) {
        Ok((Peek::Hello(v), _)) => Some(v),
        Err(_) => None,
    }
}

/// Deserializes a body returned by [`read_frame`].
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    Ok(postcard::from_bytes(body)?)
}

/// Reads one framed message body without deserializing it.
pub async fn read_frame(stream: &mut quinn::RecvStream) -> Result<Option<Vec<u8>>> {
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
    Ok(Some(body))
}

pub async fn write_msg<T: Serialize>(stream: &mut quinn::SendStream, msg: &T) -> Result<()> {
    stream.write_all(&encode(msg)?).await?;
    Ok(())
}

/// Reads one message. Returns `Ok(None)` when the peer cleanly finished the stream.
pub async fn read_msg<T: DeserializeOwned>(stream: &mut quinn::RecvStream) -> Result<Option<T>> {
    match read_frame(stream).await? {
        Some(body) => Ok(Some(postcard::from_bytes(&body)?)),
        None => Ok(None),
    }
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

    #[test]
    fn peeks_version_of_any_hello() {
        let hello = ClientMsg::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_id: "id".into(),
            client_name: "name".into(),
            auth: Auth::Pake(vec![1; 33]),
            display: DisplayRequest { width: 1920, height: 1080, refresh_hz: 60, hidpi: false, size_mm: None },
            codecs: vec![Codec::H265, Codec::H264],
            kvm: None,
            input_only: false,
            screen: Some(ScreenId { key: "\\\\.\\DISPLAY2".into(), name: "DISPLAY2".into() }),
        });
        let bytes = encode(&hello).unwrap();
        assert_eq!(peek_hello_protocol(&bytes[4..]), Some(PROTOCOL_VERSION));
        // A version 1 hello (no codecs field) still yields its version.
        let mut v1 = postcard::to_stdvec(&(0u8, 1u32, "id", "name")).unwrap();
        v1.extend_from_slice(&[0, 0]);
        assert_eq!(peek_hello_protocol(&v1), Some(1));
        assert_eq!(peek_hello_protocol(&[1, 2]), None);
    }

    #[test]
    fn hevc_nal_types() {
        // VPS header bytes 0x40 0x01, IDR_W_RADL 0x26 0x01.
        assert_eq!(Codec::H265.nal_type(&[0x40, 0x01]), 32);
        assert!(Codec::H265.is_keyframe_nal(Codec::H265.nal_type(&[0x26, 0x01])));
        assert!(!Codec::H265.is_keyframe_nal(Codec::H265.nal_type(&[0x02, 0x01])));
        assert_eq!(Codec::H264.nal_type(&[0x65]), 5);
    }
}

//! `--self-test`: decodes a small embedded clip with this machine's real decoder, draws it
//! with the real renderer into an off-screen image, and checks the colours. Needs no window,
//! so it also works over SSH. Runs twice: zero copy (where available), then copying.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::Codec;

use crate::decode::{self, Deliver, YuvFrame};
use crate::render::Renderer;

/// 30 frames of four vertical bars (red, green, blue, white), H.264 High profile, BT.709
/// limited range: what the host sends.
const CLIP: &[u8] = include_bytes!("../testdata/bars-640x360.h264");
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const BARS: [(&str, [u8; 3]); 4] =
    [("red", [255, 0, 0]), ("green", [0, 255, 0]), ("blue", [0, 0, 255]), ("white", [255, 255, 255])];
/// Per channel; video compression and the YUV round trip move colours a little.
const TOLERANCE: i32 = 40;

pub fn run() -> Result<()> {
    println!("Freemonplay Screen self-test: decode and draw a test clip with this machine's GPU\n");
    let mut failures = 0;
    for zero_copy in [true, false] {
        if !zero_copy {
            // SAFETY: single-threaded at this point; read by the renderer when it's created.
            unsafe { std::env::set_var("FMP_ZERO_COPY", "0") };
        }
        let label = if zero_copy { "zero copy allowed" } else { "copying forced" };
        match pass() {
            Ok(true) => println!("  PASS ({label})\n"),
            Ok(false) => {
                println!("  FAIL ({label})\n");
                failures += 1;
            }
            Err(e) => {
                println!("  ERROR ({label}): {e:#}\n");
                failures += 1;
            }
        }
    }
    // The link to the internet's default route (nothing is sent), as a stand-in for the
    // link to a host: usually the same Wi-Fi or cable.
    match crate::link::tune(std::net::Ipv4Addr::new(8, 8, 8, 8).into()) {
        (tuning, Some(info)) => {
            println!("Network: {}", info.summary);
            for tip in &info.advice {
                println!("  Tip: {tip}");
            }
            if tuning.is_none() && info.summary.starts_with("Wi-Fi") {
                println!("  (couldn't switch on low-latency mode)");
            }
            println!();
        }
        (_, None) => println!("Network: not inspected on this OS yet\n"),
    }
    if failures > 0 {
        bail!("{failures} of 2 passes failed");
    }
    println!("All passed.");
    Ok(())
}

fn pass() -> Result<bool> {
    let mut renderer = Renderer::new_headless(WIDTH, HEIGHT)?;
    // Only the newest picture is kept, as in a real session: shared frames each hold one of
    // the decoder's few texture slots until they're dropped.
    let latest: Arc<Mutex<(usize, Option<YuvFrame>)>> = Arc::default();
    let out_pts: Arc<Mutex<Vec<u64>>> = Arc::default();
    let deliver: Deliver = {
        let (latest, out_pts) = (latest.clone(), out_pts.clone());
        Arc::new(move |f: YuvFrame| {
            out_pts.lock().unwrap().push(f.pts_us);
            let mut latest = latest.lock().unwrap();
            latest.0 += 1;
            latest.1 = Some(f);
        })
    };
    let mut decoder = decode::new_decoder(Codec::H264, deliver, renderer.gpu_adapter())?;
    println!("  decoder: {}", decoder.name());

    let units = split_access_units(CLIP);
    let mut decode_times = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let before = latest.lock().unwrap().0;
        let start = Instant::now();
        decoder.decode(unit, pts_of(i))?;
        // Some decoders hand pictures over asynchronously; give each one a moment.
        while latest.lock().unwrap().0 == before && start.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_micros(200));
        }
        if latest.lock().unwrap().0 > before {
            decode_times.push(start.elapsed());
        }
    }
    let (decoded, last) = std::mem::take(&mut *latest.lock().unwrap());
    println!("  decoded {decoded} of {} frames", units.len());
    let last = last.context("the decoder produced no pictures")?;
    if let Some(avg) = average(&decode_times) {
        println!("  decode time per frame: {:.2} ms average", avg.as_secs_f64() * 1e3);
    }

    renderer.upload(&last);
    renderer.render(None);
    println!("  decoder output: {}", last.planes.describe());
    let path = if renderer.drew_zero_copy() { "zero copy" } else { "copied into GPU textures" };
    println!("  display path: {path}");
    let pixels = renderer.read_back()?;
    let mut ok = decoded == units.len();
    // Latency stats match pictures to arrivals by timestamp, so they must come through intact.
    let out_pts = std::mem::take(&mut *out_pts.lock().unwrap());
    let kept = out_pts.iter().filter(|&&p| (0..units.len()).any(|i| pts_of(i) == p)).count();
    let pts_ok = kept == out_pts.len();
    ok &= pts_ok;
    println!(
        "  timestamps kept: {kept} of {} {}",
        out_pts.len(),
        if pts_ok { "ok" } else { "WRONG (latency can't be measured)" }
    );
    if !pts_ok {
        println!("    sent {:?}...\n    got  {:?}...", (0..4).map(pts_of).collect::<Vec<_>>(), &out_pts[..out_pts.len().min(4)]);
    }
    for (i, (name, want)) in BARS.iter().enumerate() {
        let x = (i as u32 * 2 + 1) * WIDTH / 8;
        let at = ((HEIGHT / 2 * WIDTH + x) * 4) as usize;
        let got = [pixels[at], pixels[at + 1], pixels[at + 2]];
        let close = got.iter().zip(want).all(|(&g, &w)| (g as i32 - w as i32).abs() <= TOLERANCE);
        ok &= close;
        println!("  {name:>5} bar: got {got:?}, expected {want:?} {}", if close { "ok" } else { "WRONG" });
    }
    tracing::debug!("self-test pass finished; cleaning up");
    // As when a session ends: the decoder goes first while the renderer still shows its
    // last picture.
    drop(decoder);
    tracing::debug!("decoder dropped");
    drop(last);
    tracing::debug!("frame dropped");
    renderer.clear_video();
    tracing::debug!("renderer cleared");
    drop(renderer);
    tracing::debug!("cleaned up");
    Ok(ok)
}

/// Like the host's: microseconds since some start, at 30 fps.
fn pts_of(frame: usize) -> u64 {
    5_000_000_000 + frame as u64 * 33_333
}

fn average(times: &[Duration]) -> Option<Duration> {
    (!times.is_empty()).then(|| times.iter().sum::<Duration>() / times.len() as u32)
}

/// Splits an Annex-B stream at access unit delimiters (NAL type 9).
fn split_access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = data
        .windows(5)
        .enumerate()
        .filter(|(_, w)| w[..4] == [0, 0, 0, 1] && w[4] & 0x1F == 9)
        .map(|(i, _)| i)
        .collect();
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_has_thirty_access_units() {
        assert_eq!(split_access_units(CLIP).len(), 30);
    }
}

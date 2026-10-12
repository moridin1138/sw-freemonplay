//! Adaptive bitrate: lowers the encoder's bitrate when the network can't keep up, and
//! creeps back up once it can.
//!
//! The main signal is the sender stalling: we cap how much video QUIC may buffer (see
//! [`send_window`]), so a link slower than the bitrate makes writes wait instead of
//! silently queueing seconds of video. Packet loss is the second signal.

use std::time::{Duration, Instant};

/// How often the session feeds the controller.
pub const TICK: Duration = Duration::from_millis(500);
/// Never go below this (or below `max / MIN_FRACTION`, if larger): past that point the
/// picture is too poor to be worth it, and keyframes alone would stall.
const FLOOR_BPS: u32 = 1_500_000;
const MIN_FRACTION: u32 = 8;
/// Cut by this much when congested, at most once per `DECREASE_GAP`.
const DECREASE: f64 = 0.7;
const DECREASE_GAP: Duration = Duration::from_secs(1);
/// Raise by this much after `CALM` without trouble.
const INCREASE: f64 = 1.1;
const CALM: Duration = Duration::from_secs(3);
/// Congested when sending stalled for more than this share of the tick...
const STALLED_SHARE: f64 = 0.2;
/// ...or more than this share of packets was lost (with enough packets to tell).
const LOSS_SHARE: f64 = 0.05;
const LOSS_MIN_PACKETS: u64 = 100;
/// Ignore the start of a session: the first keyframe and the first full-screen paint are
/// bursts that briefly stall even a fast link.
const GRACE: Duration = Duration::from_secs(2);

/// What happened during one tick.
pub struct Sample {
    pub elapsed: Duration,
    /// Time spent waiting for QUIC to accept video (keyframes excluded: they're big by
    /// design and briefly stall even a healthy link).
    pub stalled: Duration,
    /// Totals since the connection started, from quinn's path stats.
    pub sent_packets: u64,
    pub lost_packets: u64,
}

pub struct RateControl {
    started: Instant,
    max: u32,
    min: u32,
    current: u32,
    last_decrease: Option<Instant>,
    calm_since: Instant,
    previous: Option<(u64, u64)>,
    loss: f64,
}

impl RateControl {
    pub fn new(max: u32, now: Instant) -> Self {
        let min = (max / MIN_FRACTION).max(FLOOR_BPS).min(max);
        Self { started: now, max, min, current: max, last_decrease: None, calm_since: now, previous: None, loss: 0.0 }
    }

    pub fn current(&self) -> u32 {
        self.current
    }

    pub fn max(&self) -> u32 {
        self.max
    }

    /// Share of packets lost in the last tick (0 to 1).
    pub fn loss(&self) -> f64 {
        self.loss
    }

    /// Returns the new bitrate when it changes.
    pub fn update(&mut self, now: Instant, s: &Sample) -> Option<u32> {
        let (sent, lost) = match self.previous.replace((s.sent_packets, s.lost_packets)) {
            Some((sent0, lost0)) => (s.sent_packets.saturating_sub(sent0), s.lost_packets.saturating_sub(lost0)),
            None => (0, 0),
        };
        self.loss = if sent > 0 { lost as f64 / sent as f64 } else { 0.0 };
        if now.duration_since(self.started) < GRACE {
            return None;
        }
        let stalled = !s.elapsed.is_zero() && s.stalled.as_secs_f64() / s.elapsed.as_secs_f64() > STALLED_SHARE;
        let lossy = sent >= LOSS_MIN_PACKETS && self.loss > LOSS_SHARE;

        if stalled || lossy {
            self.calm_since = now;
            let recently = self.last_decrease.is_some_and(|t| now.duration_since(t) < DECREASE_GAP);
            if recently || self.current == self.min {
                return None;
            }
            self.last_decrease = Some(now);
            self.current = ((self.current as f64 * DECREASE).round() as u32).max(self.min);
            return Some(self.current);
        }
        if self.current < self.max && now.duration_since(self.calm_since) >= CALM {
            self.calm_since = now;
            self.current = ((self.current as f64 * INCREASE).round() as u32).min(self.max);
            return Some(self.current);
        }
        None
    }
}

/// How much unacknowledged video QUIC may hold: the bitrate over one round trip plus a
/// little slack. Enough to fill the link, small enough that a slow link stalls the sender
/// (and so lowers the bitrate) within a frame or two instead of queueing seconds of video.
pub fn send_window(bitrate: u32, rtt: Duration) -> u64 {
    let bytes = bitrate as f64 / 8.0 * (rtt.as_secs_f64() + 0.06);
    (bytes as u64).clamp(128 * 1024, 4 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(stalled_ms: u64, sent: u64, lost: u64) -> Sample {
        Sample { elapsed: TICK, stalled: Duration::from_millis(stalled_ms), sent_packets: sent, lost_packets: lost }
    }

    #[test]
    fn backs_off_when_stalled_then_recovers() {
        let t0 = Instant::now();
        let mut rc = RateControl::new(30_000_000, t0);
        // The start-up burst is ignored.
        assert_eq!(rc.update(t0 + TICK, &sample(400, 0, 0)), None);
        let t0 = t0 + GRACE;
        // Stalled half the time: cut once, not again within a second.
        assert_eq!(rc.update(t0 + TICK, &sample(250, 0, 0)), Some(21_000_000));
        assert_eq!(rc.update(t0 + TICK * 2, &sample(250, 0, 0)), None);
        assert_eq!(rc.update(t0 + TICK * 4, &sample(250, 0, 0)), Some(14_700_000));
        // Calm for 3 s: up 10%.
        assert_eq!(rc.update(t0 + TICK * 6, &sample(0, 0, 0)), None);
        assert_eq!(rc.update(t0 + TICK * 10, &sample(0, 0, 0)), Some(16_170_000));
        assert_eq!(rc.current(), 16_170_000);
    }

    #[test]
    fn stays_between_floor_and_max() {
        let t0 = Instant::now();
        let mut rc = RateControl::new(10_000_000, t0);
        let mut t = t0;
        for _ in 0..40 {
            t += Duration::from_secs(2);
            rc.update(t, &sample(400, 0, 0));
        }
        assert_eq!(rc.current(), 1_500_000);
        for _ in 0..200 {
            t += Duration::from_secs(4);
            rc.update(t, &sample(0, 0, 0));
        }
        assert_eq!(rc.current(), 10_000_000);
    }

    #[test]
    fn loss_counts_only_with_enough_packets() {
        let t0 = Instant::now();
        let mut rc = RateControl::new(20_000_000, t0);
        let t0 = t0 + GRACE;
        rc.update(t0, &sample(0, 1000, 0));
        // 10 of 50 lost: too few packets to judge.
        assert_eq!(rc.update(t0 + TICK, &sample(0, 1050, 10)), None);
        // 20 of 200 lost (10%): congested.
        assert_eq!(rc.update(t0 + TICK * 4, &sample(0, 1250, 30)), Some(14_000_000));
        assert!((rc.loss() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn window_scales_with_rate_and_round_trip() {
        assert_eq!(send_window(1_000_000, Duration::ZERO), 128 * 1024);
        assert_eq!(send_window(30_000_000, Duration::from_millis(20)), 300_000);
        assert_eq!(send_window(200_000_000, Duration::from_millis(500)), 4 * 1024 * 1024);
    }
}

//! Non-human motion filter v0 (plan item 1.5).
//!
//! In real homes, pets, fans and robot vacuums cause most false alarms. A
//! fan is the easy case: its blades chop a reflection at a fixed rate, which
//! puts one narrow, steady line in the amplitude spectrum of every
//! subcarrier. People do not hold a narrow line in 0.5-10 Hz for long:
//! walking spreads energy across the band, and breathing is below it.
//!
//! [`PeriodicDetector`] looks for that line once per second over the last
//! 5 s of amplitudes (100 frames at 20 Hz). When a strong line stays at the
//! same frequency for [`STEADY_SECS`], the node is marked as having a
//! periodic interferer, and the motion classifier subtracts its whole
//! baseline instead of 70% of it, so the steady machine motion is removed
//! while a person's added motion still shows. A fan whose motion is far
//! above the presence threshold ([`MASKING_BASELINE_RATIO`]) would hide a
//! person too, so it is only reported: the node keeps reading occupied. Pets and robot vacuums are
//! not periodic and are not handled here (v1, which needs labelled data).
//!
//! Thresholds are a starting point tuned on SYNTHETIC rooms; not yet
//! MEASURED on hardware.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// Frames analysed per check: the whole node history (5 s at 20 Hz).
pub const WINDOW_FRAMES: usize = 100;
/// Every n-th subcarrier is analysed; the line is common to all of them.
const SUBCARRIER_STRIDE: usize = 4;
/// Lowest frequency searched, Hz (above breathing).
const MIN_HZ: f64 = 0.5;
/// Peak-to-median ratio of the summed spectrum that counts as a line.
pub const LINE_RATIO: f64 = 8.0;
/// Seconds the line must hold its frequency before the node is flagged.
pub const STEADY_SECS: u32 = 10;
/// Seconds without a line before the flag is dropped.
const CLEAR_SECS: u32 = 10;
/// A flagged interferer whose motion baseline exceeds this multiple of the
/// presence threshold is too strong to remove: removing it would also hide
/// a person (SYNTHETIC: typical fans sit at 1.8-3.0x, a fan stronger than
/// the person's reflection at 3.0-4.6x).
pub const MASKING_BASELINE_RATIO: f64 = 3.5;

/// The strongest spectral line in the amplitude history: (peak-to-median
/// ratio, frequency in Hz, bin). `None` without enough history.
pub fn spectral_line(history: &VecDeque<Vec<f64>>, fps: f64) -> Option<(f64, f64, usize)> {
    if history.len() < WINDOW_FRAMES || !(fps.is_finite() && fps > 2.0 * MIN_HZ) {
        return None;
    }
    let start = history.len() - WINDOW_FRAMES;
    let width = history[start].len();
    if width == 0 || history.iter().skip(start).any(|f| f.len() != width) {
        return None;
    }
    let n = WINDOW_FRAMES;
    let lo = ((MIN_HZ * n as f64 / fps).ceil() as usize).max(1);
    let hi = n / 2 - 1;
    if lo >= hi {
        return None;
    }
    let window: Vec<f64> = (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n as f64 - 1.0)).cos()))
        .collect();
    let (sin, cos): (Vec<f64>, Vec<f64>) = (0..n)
        .map(|i| (2.0 * PI * i as f64 / n as f64).sin_cos())
        .unzip();
    let mut power = vec![0.0; hi + 1];
    for j in (0..width).step_by(SUBCARRIER_STRIDE) {
        let x: Vec<f64> = history
            .iter()
            .skip(start)
            .map(|f| f[j].max(1e-3).ln())
            .collect();
        let mean = x.iter().sum::<f64>() / n as f64;
        let xw: Vec<f64> = x.iter().zip(&window).map(|(v, w)| (v - mean) * w).collect();
        for (k, p) in power.iter_mut().enumerate().take(hi + 1).skip(lo) {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, v) in xw.iter().enumerate() {
                let idx = (k * i) % n;
                re += v * cos[idx];
                im -= v * sin[idx];
            }
            *p += re * re + im * im;
        }
    }
    let band = &power[lo..=hi];
    let (peak_off, peak) =
        band.iter().enumerate().fold(
            (0, 0.0),
            |best, (i, &p)| if p > best.1 { (i, p) } else { best },
        );
    let mut sorted = band.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    if median <= 0.0 {
        return None;
    }
    let bin = lo + peak_off;
    Some((peak / median, bin as f64 * fps / n as f64, bin))
}

/// Tracks whether a node has a steady periodic interferer.
#[derive(Debug, Default, Clone)]
pub struct PeriodicDetector {
    frames_since_check: u32,
    /// Checks that had a full window of history.
    checks: u32,
    steady_secs: u32,
    quiet_secs: u32,
    last_bin: Option<usize>,
    active: bool,
    last_ratio: f64,
    last_hz: f64,
}

impl PeriodicDetector {
    /// Call once per frame; checks once per second of frames. Returns
    /// whether a periodic interferer is present.
    pub fn update(&mut self, history: &VecDeque<Vec<f64>>, fps: f64) -> bool {
        self.update_with(history, fps, LINE_RATIO, STEADY_SECS)
    }

    #[doc(hidden)]
    pub fn update_with(
        &mut self,
        history: &VecDeque<Vec<f64>>,
        fps: f64,
        line_ratio: f64,
        steady: u32,
    ) -> bool {
        self.frames_since_check += 1;
        if f64::from(self.frames_since_check) < fps.round().max(1.0) {
            return self.active;
        }
        self.frames_since_check = 0;
        // A check counts once a full window exists, line or not: flat data
        // (no spectrum to speak of) must not keep the detector unready.
        if history.len() >= WINDOW_FRAMES {
            self.checks = self.checks.saturating_add(1);
        }
        let line = spectral_line(history, fps);
        match line {
            Some((ratio, hz, bin)) if ratio >= line_ratio => {
                self.last_ratio = ratio;
                self.last_hz = hz;
                let same = self.last_bin.is_some_and(|b| b.abs_diff(bin) <= 1);
                self.steady_secs = if same { self.steady_secs + 1 } else { 1 };
                self.last_bin = Some(bin);
                self.quiet_secs = 0;
                if self.steady_secs >= steady {
                    self.active = true;
                }
            }
            other => {
                if let Some((ratio, hz, _)) = other {
                    self.last_ratio = ratio;
                    self.last_hz = hz;
                }
                self.steady_secs = 0;
                self.last_bin = None;
                self.quiet_secs += 1;
                if self.quiet_secs >= CLEAR_SECS {
                    self.active = false;
                }
            }
        }
        self.active
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// Enough checks have run that a fan present from the start would
    /// already be suspected. Motion before this should not start long holds.
    pub fn ready(&self) -> bool {
        self.checks >= 3
    }

    /// A line has been seen at the same frequency in consecutive checks but
    /// not yet long enough to flag. Motion while suspected should not start
    /// long holds.
    pub fn suspected(&self) -> bool {
        self.active || self.steady_secs >= 2
    }

    /// Latest line strength and frequency, for diagnostics.
    pub fn last_line(&self) -> (f64, f64) {
        (self.last_ratio, self.last_hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 20.0;

    /// 52 subcarriers whose amplitude carries `tone(t)` plus noise.
    fn frame(i: usize, seed: &mut u64, tone: impl Fn(f64) -> f64) -> Vec<f64> {
        let t = i as f64 / FPS;
        (0..52)
            .map(|k| {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = ((*seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.4;
                (10.0 + k as f64 * 0.1) * (1.0 + 0.2 * tone(t)) + noise
            })
            .collect()
    }

    fn run(secs: f64, tone: impl Fn(f64) -> f64) -> (PeriodicDetector, Vec<bool>) {
        let mut det = PeriodicDetector::default();
        let mut hist = VecDeque::new();
        let mut seed = 11;
        let mut flags = Vec::new();
        for i in 0..(secs * FPS) as usize {
            hist.push_back(frame(i, &mut seed, &tone));
            if hist.len() > WINDOW_FRAMES {
                hist.pop_front();
            }
            flags.push(det.update(&hist, FPS));
        }
        (det, flags)
    }

    #[test]
    fn a_steady_fan_line_is_flagged_after_the_steady_time() {
        let (det, flags) = run(40.0, |t| (2.0 * PI * 4.2 * t).sin());
        let first = flags.iter().position(|f| *f).expect("flagged") as f64 / FPS;
        assert!((10.0..20.0).contains(&first), "flagged at {first} s");
        assert!(det.active());
        let (_, hz) = det.last_line();
        assert!((hz - 4.2).abs() < 0.3, "{hz}");
    }

    #[test]
    fn noise_is_not_flagged() {
        let (det, flags) = run(60.0, |_| 0.0);
        assert!(!flags.iter().any(|f| *f));
        assert!(!det.suspected());
    }

    #[test]
    fn a_sweeping_tone_is_not_flagged() {
        // Like a Doppler line from someone walking: the frequency keeps
        // moving, so it never holds one bin for long.
        let (_, flags) = run(60.0, |t| (2.0 * PI * (1.0 + 0.15 * t) * t).sin());
        assert!(!flags.iter().any(|f| *f));
    }

    #[test]
    fn the_flag_clears_when_the_fan_stops() {
        let (mut det, _) = run(30.0, |t| (2.0 * PI * 4.2 * t).sin());
        assert!(det.active());
        let mut hist = VecDeque::new();
        let mut seed = 5;
        let mut cleared_at = None;
        for i in 0..(30.0 * FPS) as usize {
            hist.push_back(frame(i, &mut seed, |_| 0.0));
            if hist.len() > WINDOW_FRAMES {
                hist.pop_front();
            }
            if !det.update(&hist, FPS) && cleared_at.is_none() {
                cleared_at = Some(i as f64 / FPS);
            }
        }
        let t = cleared_at.expect("cleared");
        assert!((5.0..20.0).contains(&t), "cleared at {t} s");
    }

    #[test]
    fn flat_data_still_becomes_ready() {
        let mut det = PeriodicDetector::default();
        // A full window from which no spectrum can be taken (frames without
        // subcarriers).
        let hist: VecDeque<Vec<f64>> = (0..WINDOW_FRAMES).map(|_| Vec::new()).collect();
        assert!(spectral_line(&hist, FPS).is_none());
        for _ in 0..(5.0 * FPS) as usize {
            det.update(&hist, FPS);
        }
        assert!(det.ready());
        assert!(!det.active());
    }

    #[test]
    fn ready_after_three_full_checks() {
        let mut det = PeriodicDetector::default();
        let mut hist = VecDeque::new();
        let mut seed = 1;
        let mut ready_at = None;
        for i in 0..(10.0 * FPS) as usize {
            hist.push_back(frame(i, &mut seed, |_| 0.0));
            if hist.len() > WINDOW_FRAMES {
                hist.pop_front();
            }
            det.update(&hist, FPS);
            if det.ready() && ready_at.is_none() {
                ready_at = Some(i as f64 / FPS);
            }
        }
        // A full window takes 5 s; then three once-a-second checks.
        let t = ready_at.expect("ready");
        assert!((6.0..9.0).contains(&t), "ready at {t} s");
    }
}

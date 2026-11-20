//! Vital sign detection from WiFi CSI data: the sensing server's detector.
//!
//! This is the implementation the RuView sensing server runs and Home
//! Assistant sees. It lived in `wifi-densepose-sensing-server` as
//! `vital_signs.rs` until plan item 4.1 (review R11) moved it here, so that
//! the server's vitals come from this crate. The server re-exports it
//! unchanged as `wifi_densepose_sensing_server::vital_signs`.
//!
//! Implements breathing rate (0.1-0.5 Hz) and heart rate (0.8-2.0 Hz)
//! estimation using FFT-based spectral analysis on CSI amplitude and phase
//! time series. Designed per ADR-021 (rvdna vital sign pipeline).
//!
//! Breathing (plan item 1.2) does not average the subcarriers together:
//! breathing moves some subcarriers up and others down, so the mean cancels
//! much of it. Instead every subcarrier's log-amplitude (with the per-frame
//! gain removed) and its cleaned phase (unwrapped across subcarriers, with
//! the per-packet linear offset removed) is kept as a separate stream. Once
//! per second the six streams with the most energy in the breathing band are
//! combined with PCA and the peak of the first component is the estimate.
//! In the SYNTHETIC scenario harness this took breathing error from about
//! 0.8-1.5 BPM to under 0.5 BPM; it is not yet MEASURED on hardware.
//!
//! All math is pure Rust -- no external FFT crate required. Uses a radix-2
//! DIT FFT for buffers zero-padded to power-of-two length. A windowed-sinc
//! FIR bandpass filter isolates the frequency bands of interest before
//! spectral analysis.

// Moved verbatim from the sensing server (plan item 4.1). These lints come
// from this crate's stricter clippy settings; tidying them is left out of the
// move so that the move stays verbatim and the scenario harness stays identical.
#![allow(
    clippy::cast_possible_wrap,
    clippy::explicit_iter_loop,
    clippy::float_cmp,
    clippy::manual_is_multiple_of,
    clippy::manual_midpoint,
    clippy::needless_for_each,
    clippy::uninlined_format_args,
    clippy::unreadable_literal
)]

use std::collections::VecDeque;
use std::f64::consts::PI;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

// ── Configuration constants ────────────────────────────────────────────────

/// Breathing rate physiological band: 6-30 breaths per minute.
const BREATHING_MIN_HZ: f64 = 0.1; // 6 BPM
const BREATHING_MAX_HZ: f64 = 0.5; // 30 BPM

/// Heart rate physiological band: 40-120 beats per minute.
const HEARTBEAT_MIN_HZ: f64 = 0.667; // 40 BPM
const HEARTBEAT_MAX_HZ: f64 = 2.0; // 120 BPM

/// Minimum number of samples before attempting extraction.
const MIN_BREATHING_SAMPLES: usize = 40; // ~2s at 20 Hz
const MIN_HEARTBEAT_SAMPLES: usize = 30; // ~1.5s at 20 Hz

/// Peak-to-mean ratio threshold for confident detection.
const CONFIDENCE_THRESHOLD: f64 = 2.0;

/// Breathing streams are block-averaged down to about this rate before the
/// spectral work. Breathing is below 0.5 Hz, so 4 Hz keeps it with room to
/// spare and cuts the per-second work by the decimation factor.
const BREATHING_STREAM_HZ: f64 = 4.0;
/// Seconds of decimated history needed before the first breathing estimate:
/// one full cycle at 6 BPM.
const MIN_BREATHING_SECS: f64 = 10.0;
/// At most this many streams are combined by PCA, ranked by breathing-band
/// energy.
const BREATHING_TOP_STREAMS: usize = 6;
/// Only streams whose band-energy share is at least this fraction of the
/// best stream's are combined. When one stream carries the breathing and the
/// rest are noise, PCA over equally weighted noise streams loses it.
const BREATHING_STREAM_KEEP: f64 = 0.75;
/// Breathing evidence (plan item 1.3) is measured on this many of the most
/// recent seconds, over this many top-ranked streams.
const EVIDENCE_WINDOW_SECS: f64 = 20.0;
const EVIDENCE_STREAMS: usize = 8;
/// If the spectrum at half the peak frequency reaches this share of the
/// peak, the half frequency is taken: amplitude breathing often shows a
/// strong second harmonic when the reflection sits near a Fresnel-zone edge.
const BREATHING_SUBHARMONIC_SHARE: f64 = 0.5;
/// Zero-padding factor for the final breathing spectrum.
const BREATHING_FFT_PAD: usize = 4;

// ── Output types ───────────────────────────────────────────────────────────

/// Vital sign readings produced each frame.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct VitalSigns {
    /// Estimated breathing rate in breaths per minute, if detected.
    pub breathing_rate_bpm: Option<f64>,
    /// Estimated heart rate in beats per minute, if detected.
    pub heart_rate_bpm: Option<f64>,
    /// Confidence of breathing estimate (0.0 - 1.0).
    pub breathing_confidence: f64,
    /// Confidence of heartbeat estimate (0.0 - 1.0).
    pub heartbeat_confidence: f64,
    /// Overall signal quality metric (0.0 - 1.0).
    pub signal_quality: f64,
}

impl Default for VitalSigns {
    fn default() -> Self {
        Self {
            breathing_rate_bpm: None,
            heart_rate_bpm: None,
            breathing_confidence: 0.0,
            heartbeat_confidence: 0.0,
            signal_quality: 0.0,
        }
    }
}

// ── Detector ───────────────────────────────────────────────────────────────

/// Stateful vital sign detector. Maintains rolling buffers of CSI amplitude
/// data and extracts breathing and heart rate via spectral analysis.
#[allow(dead_code)]
pub struct VitalSignDetector {
    /// Rolling buffer of mean-amplitude samples. Drives the signal-quality
    /// fill factor and `buffer_status`; breathing itself is estimated from
    /// `breathing_streams`.
    breathing_buffer: VecDeque<f64>,
    /// Per-subcarrier breathing streams, block-averaged to about
    /// [`BREATHING_STREAM_HZ`].
    breathing_streams: BreathingStreams,
    /// Rolling buffer of phase-variance samples for heartbeat detection.
    heartbeat_buffer: VecDeque<f64>,
    /// CSI frame arrival rate in Hz.
    sample_rate: f64,
    /// Window duration for breathing FFT in seconds.
    breathing_window_secs: f64,
    /// Window duration for heartbeat FFT in seconds.
    heartbeat_window_secs: f64,
    /// Maximum breathing buffer capacity (samples).
    breathing_capacity: usize,
    /// Maximum heartbeat buffer capacity (samples).
    heartbeat_capacity: usize,
    /// Running frame count for signal quality estimation.
    frame_count: u64,
    /// Frames between spectral estimates: one second of CSI at the
    /// configured sample rate. The windows are 15 and 30 s long, so running
    /// the band-pass filters and FFTs on every frame (about 75% of per-frame
    /// time at 20 Hz) recomputed almost the same spectrum each time.
    estimate_interval_frames: u64,
    /// Frames pushed since the last spectral estimate.
    frames_since_estimate: u64,
    /// Last spectral estimate: (breathing, breathing confidence, heart rate,
    /// heartbeat confidence). `None` until the first frame is processed.
    last_estimate: Option<(Option<f64>, f64, Option<f64>, f64)>,
    /// Breathing evidence from the last spectral estimate.
    last_breathing_evidence: Option<f64>,
    /// Counts spectral estimates, so callers can tell a fresh evidence
    /// value from a repeated one.
    estimates: u64,
}

/// Spectral estimates are refreshed once per this many seconds of CSI.
const ESTIMATE_INTERVAL_SECS: f64 = 1.0;

fn estimate_interval_frames(sample_rate: f64) -> u64 {
    ((sample_rate * ESTIMATE_INTERVAL_SECS).round() as u64).max(1)
}

impl VitalSignDetector {
    /// Create a new detector with the given CSI sample rate (Hz).
    ///
    /// Typical sample rates:
    /// - ESP32 CSI: 20-100 Hz
    /// - Windows WiFi RSSI: 2 Hz (insufficient for heartbeat)
    /// - Simulation: 2-20 Hz
    pub fn new(sample_rate: f64) -> Self {
        let breathing_window_secs = 30.0;
        let heartbeat_window_secs = 15.0;
        let breathing_capacity = (sample_rate * breathing_window_secs) as usize;
        let heartbeat_capacity = (sample_rate * heartbeat_window_secs) as usize;

        Self {
            breathing_buffer: VecDeque::with_capacity(breathing_capacity.max(1)),
            breathing_streams: BreathingStreams::new(sample_rate, breathing_window_secs),
            heartbeat_buffer: VecDeque::with_capacity(heartbeat_capacity.max(1)),
            sample_rate,
            breathing_window_secs,
            heartbeat_window_secs,
            breathing_capacity: breathing_capacity.max(1),
            heartbeat_capacity: heartbeat_capacity.max(1),
            frame_count: 0,
            estimate_interval_frames: estimate_interval_frames(sample_rate),
            frames_since_estimate: 0,
            last_estimate: None,
            last_breathing_evidence: None,
            estimates: 0,
        }
    }

    /// Process one CSI frame and return updated vital signs.
    ///
    /// `amplitude` - per-subcarrier amplitude values for this frame.
    /// `phase` - per-subcarrier phase values for this frame.
    ///
    /// The detector extracts two aggregate features per frame:
    /// 1. Mean amplitude (breathing signal -- chest movement modulates path loss)
    /// 2. Phase variance across subcarriers (heartbeat signal -- subtle phase shifts)
    ///
    /// Both features are buffered on every frame. The breathing and heart-rate
    /// spectra are recomputed once per second of CSI (see
    /// [`ESTIMATE_INTERVAL_SECS`]); frames in between return the latest
    /// estimate with a freshly computed signal quality.
    pub fn process_frame(&mut self, amplitude: &[f64], phase: &[f64]) -> VitalSigns {
        self.frame_count += 1;

        if amplitude.is_empty() {
            return VitalSigns::default();
        }

        // -- Feature 1: Mean amplitude for breathing detection --
        // Respiratory chest displacement (1-5 mm) modulates CSI amplitudes
        // across all subcarriers. Mean amplitude captures this well.
        let n = amplitude.len() as f64;
        let mean_amp: f64 = amplitude.iter().sum::<f64>() / n;

        // A new subcarrier layout restarts the breathing streams; restart
        // the mean-amplitude history and cached estimate with them, so the
        // buffer status and signal-quality fill do not report history that
        // breathing can no longer use.
        if self.breathing_streams.push(amplitude, phase) {
            self.breathing_buffer.clear();
            self.last_estimate = None;
        }
        self.breathing_buffer.push_back(mean_amp);
        while self.breathing_buffer.len() > self.breathing_capacity {
            self.breathing_buffer.pop_front();
        }

        // -- Feature 2: Phase variance for heartbeat detection --
        // Cardiac-induced body surface displacement is < 0.5 mm, producing
        // tiny phase changes. Cross-subcarrier phase variance captures this
        // more sensitively than amplitude alone.
        //
        // Phases come from atan2() and are wrapped to (-pi, pi]. Linear mean
        // and variance on wrapped values is wrong: two phases close across
        // the +/-pi discontinuity (e.g. pi-eps and -pi+eps) are physically
        // ~2*eps rad apart but produce arithmetic variance ~pi^2. Use the
        // standard circular variance (1 - mean resultant length), which is
        // stable across the wrap.
        let phase_var = if phase.len() > 1 {
            phase_circular_variance(phase)
        } else {
            // Fallback: use amplitude high-pass residual when phase is unavailable
            let half = amplitude.len() / 2;
            if half > 0 {
                let hi_mean: f64 =
                    amplitude[half..].iter().sum::<f64>() / (amplitude.len() - half) as f64;
                amplitude[half..]
                    .iter()
                    .map(|a| (a - hi_mean).powi(2))
                    .sum::<f64>()
                    / (amplitude.len() - half) as f64
            } else {
                0.0
            }
        };

        self.heartbeat_buffer.push_back(phase_var);
        while self.heartbeat_buffer.len() > self.heartbeat_capacity {
            self.heartbeat_buffer.pop_front();
        }

        // -- Extract vital signs (once per estimate interval) --
        self.frames_since_estimate += 1;
        let (breathing_rate, breathing_confidence, heart_rate, heartbeat_confidence) =
            match self.last_estimate {
                Some(cached) if self.frames_since_estimate < self.estimate_interval_frames => cached,
                _ => {
                    let breathing = self.estimate_breathing();
                    let (br, br_conf) = (breathing.bpm, breathing.confidence);
                    self.last_breathing_evidence = breathing.evidence;
                    self.estimates += 1;
                    let (hr, hr_conf) = self.extract_heartbeat();
                    let fresh = (br, br_conf, hr, hr_conf);
                    self.last_estimate = Some(fresh);
                    self.frames_since_estimate = 0;
                    fresh
                }
            };

        // -- Signal quality --
        let signal_quality = self.compute_signal_quality(amplitude);

        VitalSigns {
            breathing_rate_bpm: breathing_rate,
            heart_rate_bpm: heart_rate,
            breathing_confidence,
            heartbeat_confidence,
            signal_quality,
        }
    }

    /// Extract breathing rate from the per-subcarrier streams: rank them by
    /// breathing-band energy, combine the top few with PCA, and take the
    /// spectral peak of the first component. Returns (rate_bpm, confidence).
    pub fn extract_breathing(&self) -> (Option<f64>, f64) {
        let e = self.estimate_breathing();
        (e.bpm, e.confidence)
    }

    fn estimate_breathing(&self) -> BreathingEstimate {
        if self.breathing_buffer.len() < MIN_BREATHING_SAMPLES {
            return BreathingEstimate::default();
        }
        self.breathing_streams.estimate()
    }

    /// How periodic the breathing band is right now: across the streams with
    /// the most breathing-band energy, the mean ratio of the band's spectral
    /// peak to the median of 0.5-2 Hz, over the last 20 s. A still, breathing
    /// person scores far higher than an empty room or a walking person
    /// (SYNTHETIC: empty rooms up to 19, a seated person typically 130).
    /// Updated once per second; `None` until 20 s of CSI.
    pub fn breathing_evidence(&self) -> Option<f64> {
        self.last_breathing_evidence
    }

    /// Number of spectral estimates so far; changes when
    /// [`Self::breathing_evidence`] is refreshed.
    pub fn estimate_count(&self) -> u64 {
        self.estimates
    }

    /// Extract heart rate from the heartbeat buffer via FFT.
    /// Returns (rate_bpm, confidence).
    pub fn extract_heartbeat(&self) -> (Option<f64>, f64) {
        if self.heartbeat_buffer.len() < MIN_HEARTBEAT_SAMPLES {
            return (None, 0.0);
        }

        let data: Vec<f64> = self.heartbeat_buffer.iter().copied().collect();
        let filtered = bandpass_filter(&data, HEARTBEAT_MIN_HZ, HEARTBEAT_MAX_HZ, self.sample_rate);
        self.compute_fft_peak(&filtered, HEARTBEAT_MIN_HZ, HEARTBEAT_MAX_HZ)
    }

    /// Find the dominant frequency in `buffer` within the [min_hz, max_hz] band
    /// using FFT. Returns (frequency_as_bpm, confidence).
    pub fn compute_fft_peak(&self, buffer: &[f64], min_hz: f64, max_hz: f64) -> (Option<f64>, f64) {
        if buffer.len() < 4 {
            return (None, 0.0);
        }

        // Zero-pad to next power of two for radix-2 FFT
        let fft_len = buffer.len().next_power_of_two();
        let mut signal = vec![0.0; fft_len];
        signal[..buffer.len()].copy_from_slice(buffer);

        // Apply Hann window to reduce spectral leakage
        #[allow(clippy::needless_range_loop)]
        for i in 0..buffer.len() {
            let w = 0.5 * (1.0 - (2.0 * PI * i as f64 / (buffer.len() as f64 - 1.0)).cos());
            signal[i] *= w;
        }

        // Compute FFT magnitude spectrum
        let spectrum = fft_magnitude(&signal);

        // Frequency resolution
        let freq_res = self.sample_rate / fft_len as f64;

        // Find bin range for our band of interest
        let min_bin = (min_hz / freq_res).ceil() as usize;
        let max_bin = ((max_hz / freq_res).floor() as usize).min(spectrum.len().saturating_sub(1));

        if min_bin >= max_bin || min_bin >= spectrum.len() {
            return (None, 0.0);
        }

        // Find peak magnitude and its bin index within the band
        let mut peak_mag = 0.0f64;
        let mut peak_bin = min_bin;
        let mut band_sum = 0.0f64;
        let mut band_count = 0usize;

        #[allow(clippy::needless_range_loop)]
        for bin in min_bin..=max_bin {
            let mag = spectrum[bin];
            band_sum += mag;
            band_count += 1;
            if mag > peak_mag {
                peak_mag = mag;
                peak_bin = bin;
            }
        }

        if band_count == 0 || band_sum < f64::EPSILON {
            return (None, 0.0);
        }

        let band_mean = band_sum / band_count as f64;

        // Confidence: ratio of peak to band mean, normalized to 0-1
        let peak_ratio = if band_mean > f64::EPSILON {
            peak_mag / band_mean
        } else {
            0.0
        };

        // Parabolic interpolation for sub-bin frequency accuracy
        let peak_freq = if peak_bin > min_bin && peak_bin < max_bin {
            let alpha = spectrum[peak_bin - 1];
            let beta = spectrum[peak_bin];
            let gamma = spectrum[peak_bin + 1];
            let denom = alpha - 2.0 * beta + gamma;
            if denom.abs() > f64::EPSILON {
                let p = 0.5 * (alpha - gamma) / denom;
                (peak_bin as f64 + p) * freq_res
            } else {
                peak_bin as f64 * freq_res
            }
        } else {
            peak_bin as f64 * freq_res
        };

        let bpm = peak_freq * 60.0;

        // Confidence mapping: peak_ratio >= CONFIDENCE_THRESHOLD maps to high confidence
        let confidence = peak_ratio_confidence(peak_ratio);

        if confidence > 0.05 {
            (Some(bpm), confidence)
        } else {
            (None, confidence)
        }
    }

    /// Overall signal quality based on amplitude statistics.
    fn compute_signal_quality(&self, amplitude: &[f64]) -> f64 {
        if amplitude.is_empty() {
            return 0.0;
        }

        let n = amplitude.len() as f64;
        let mean = amplitude.iter().sum::<f64>() / n;

        if mean < f64::EPSILON {
            return 0.0;
        }

        let variance = amplitude.iter().map(|a| (a - mean).powi(2)).sum::<f64>() / n;
        let cv = variance.sqrt() / mean; // coefficient of variation

        // Good signal: moderate CV (some variation from body motion, not pure noise).
        // - Too low CV (~0) = static, no person present
        // - Too high CV (>1) = noisy/unstable signal
        // Sweet spot around 0.05-0.3
        let quality = if cv < 0.01 {
            cv / 0.01 * 0.3 // very low variation => low quality
        } else if cv < 0.3 {
            0.3 + 0.7 * (1.0 - ((cv - 0.15) / 0.15).abs()).max(0.0) // peak around 0.15
        } else {
            (1.0 - (cv - 0.3) / 0.7).clamp(0.1, 0.5) // too noisy
        };

        // Factor in buffer fill level (need enough history for reliable estimates)
        let fill = (self.breathing_buffer.len() as f64) / (self.breathing_capacity as f64).max(1.0);
        let fill_factor = fill.clamp(0.0, 1.0);

        (quality * (0.3 + 0.7 * fill_factor)).clamp(0.0, 1.0)
    }

    /// Clear all internal buffers and reset state.
    pub fn reset(&mut self) {
        self.breathing_buffer.clear();
        self.breathing_streams.clear();
        self.heartbeat_buffer.clear();
        self.frame_count = 0;
        self.frames_since_estimate = 0;
        self.last_estimate = None;
        self.last_breathing_evidence = None;
    }

    /// Retune temporal windows to an observed CSI clock. Changes smaller than
    /// twenty percent are ignored to avoid resetting on normal arrival jitter.
    /// A real clock change clears history so samples from different clocks are
    /// never interpreted in one spectrum.
    pub fn reconfigure_sample_rate(&mut self, sample_rate: f64) -> bool {
        if !sample_rate.is_finite() || sample_rate < 1.0 {
            return false;
        }
        let relative_change = (sample_rate - self.sample_rate).abs() / self.sample_rate.max(1.0);
        if relative_change < 0.20 {
            return false;
        }
        self.sample_rate = sample_rate;
        self.breathing_capacity =
            ((sample_rate * self.breathing_window_secs) as usize).max(1);
        self.heartbeat_capacity =
            ((sample_rate * self.heartbeat_window_secs) as usize).max(1);
        self.estimate_interval_frames = estimate_interval_frames(sample_rate);
        self.breathing_streams = BreathingStreams::new(sample_rate, self.breathing_window_secs);
        self.reset();
        true
    }

    pub fn sample_rate_hz(&self) -> f64 {
        self.sample_rate
    }

    /// Current buffer fill levels for diagnostics.
    /// Returns (breathing_len, breathing_capacity, heartbeat_len, heartbeat_capacity).
    pub fn buffer_status(&self) -> (usize, usize, usize, usize) {
        (
            self.breathing_buffer.len(),
            self.breathing_capacity,
            self.heartbeat_buffer.len(),
            self.heartbeat_capacity,
        )
    }
}

// ── Breathing streams (plan item 1.2) ─────────────────────────────────────

/// One breathing estimate.
#[derive(Debug, Default, Clone, Copy)]
struct BreathingEstimate {
    bpm: Option<f64>,
    confidence: f64,
    /// See [`VitalSignDetector::breathing_evidence`].
    evidence: Option<f64>,
}

/// Rolling, block-averaged history of one feature vector per frame:
/// per-subcarrier log-amplitude with the frame's mean removed, the frame's
/// mean log-amplitude itself, and per-subcarrier cleaned phase.
struct BreathingStreams {
    /// Frames averaged into one stored sample.
    decimation: usize,
    /// Rate of the stored samples, Hz.
    rate_hz: f64,
    capacity: usize,
    samples: VecDeque<Vec<f64>>,
    block_sum: Vec<f64>,
    block_frames: usize,
}

impl BreathingStreams {
    fn new(sample_rate: f64, window_secs: f64) -> Self {
        let decimation = ((sample_rate / BREATHING_STREAM_HZ).round() as usize).max(1);
        let rate_hz = sample_rate / decimation as f64;
        Self {
            decimation,
            rate_hz,
            capacity: ((window_secs * rate_hz).round() as usize).max(1),
            samples: VecDeque::new(),
            block_sum: Vec::new(),
            block_frames: 0,
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.block_sum.clear();
        self.block_frames = 0;
    }

    /// Add one frame. Returns true when the frame's layout differs from the
    /// stored history, which is then discarded.
    fn push(&mut self, amplitude: &[f64], phase: &[f64]) -> bool {
        let features = breathing_features(amplitude, phase);
        if features.is_empty() {
            return false;
        }
        // A different subcarrier layout cannot share history with the old one.
        let width = self.samples.front().map_or(self.block_sum.len(), Vec::len);
        let restarted = width != 0 && width != features.len();
        if restarted {
            self.clear();
        }
        if self.block_sum.len() != features.len() {
            self.block_sum = vec![0.0; features.len()];
        }
        for (acc, x) in self.block_sum.iter_mut().zip(&features) {
            *acc += x;
        }
        self.block_frames += 1;
        if self.block_frames == self.decimation {
            let n = self.block_frames as f64;
            self.samples
                .push_back(self.block_sum.iter().map(|v| v / n).collect());
            self.block_sum.iter_mut().for_each(|v| *v = 0.0);
            self.block_frames = 0;
            while self.samples.len() > self.capacity {
                self.samples.pop_front();
            }
        }
        restarted
    }

    /// Breathing rate, confidence and evidence from the stored history.
    fn estimate(&self) -> BreathingEstimate {
        let n = self.samples.len();
        if (n as f64) < MIN_BREATHING_SECS * self.rate_hz || n < 8 {
            return BreathingEstimate::default();
        }
        let width = self.samples[0].len();
        // Detrended streams, stream-major.
        let streams: Vec<Vec<f64>> = (0..width)
            .map(|j| detrend(&self.samples.iter().map(|s| s[j]).collect::<Vec<_>>()))
            .collect();

        // Rank streams by the share of their (Hann-windowed) energy that lies
        // in the breathing band. The DFT bins are evaluated directly: only the
        // dozen or so band bins are needed, not a full spectrum per stream.
        let k_lo = (BREATHING_MIN_HZ * n as f64 / self.rate_hz).ceil() as usize;
        let k_hi = ((BREATHING_MAX_HZ * n as f64 / self.rate_hz).floor() as usize).min(n / 2);
        if k_lo == 0 || k_lo > k_hi {
            return BreathingEstimate::default();
        }
        let window: Vec<f64> = (0..n)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n as f64 - 1.0)).cos()))
            .collect();
        let twiddles: Vec<Vec<(f64, f64)>> = (k_lo..=k_hi)
            .map(|k| {
                let (step_s, step_c) = (-2.0 * PI * k as f64 / n as f64).sin_cos();
                let (mut c, mut s) = (1.0_f64, 0.0_f64);
                (0..n)
                    .map(|i| {
                        let w = (window[i] * c, window[i] * s);
                        let (nc, ns) = (c * step_c - s * step_s, c * step_s + s * step_c);
                        c = nc;
                        s = ns;
                        w
                    })
                    .collect()
            })
            .collect();
        let mut ranked: Vec<(f64, usize)> = streams
            .iter()
            .enumerate()
            .filter_map(|(j, x)| {
                let total: f64 = x.iter().zip(&window).map(|(v, w)| (v * w).powi(2)).sum();
                if total <= f64::EPSILON {
                    return None;
                }
                let band: f64 = twiddles
                    .iter()
                    .map(|tw| {
                        let (re, im) = x
                            .iter()
                            .zip(tw)
                            .fold((0.0, 0.0), |(re, im), (v, (c, s))| (re + v * c, im + v * s));
                        re * re + im * im
                    })
                    .sum();
                // One-sided band energy as a share of all energy (Parseval).
                Some((2.0 * band / (n as f64 * total), j))
            })
            .collect();
        if ranked.is_empty() {
            return BreathingEstimate::default();
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        let evidence = self.evidence(&ranked);
        let keep = ranked[0].0 * BREATHING_STREAM_KEEP;
        ranked.retain(|(share, _)| *share >= keep);
        ranked.truncate(BREATHING_TOP_STREAMS);

        // z-score the chosen streams and take their first principal component.
        let chosen: Vec<Vec<f64>> = ranked
            .iter()
            .map(|(_, j)| {
                let x = &streams[*j];
                let sd = (x.iter().map(|v| v * v).sum::<f64>() / n as f64)
                    .sqrt()
                    .max(1e-12);
                x.iter().map(|v| v / sd).collect()
            })
            .collect();
        let pc = first_principal_component(&chosen);
        let (bpm, confidence) = compute_spectral_peak(
            &pc,
            self.rate_hz,
            BREATHING_MIN_HZ,
            BREATHING_MAX_HZ,
            BREATHING_FFT_PAD,
            Some(BREATHING_SUBHARMONIC_SHARE),
        );
        BreathingEstimate {
            bpm,
            confidence,
            evidence,
        }
    }

    /// Mean, over the top-ranked streams, of the breathing band's peak power
    /// divided by the median power in 0.5-2 Hz, on the last
    /// [`EVIDENCE_WINDOW_SECS`]. Scale-free, so it does not depend on the
    /// link's gain or noise level.
    fn evidence(&self, ranked: &[(f64, usize)]) -> Option<f64> {
        let take = (EVIDENCE_WINDOW_SECS * self.rate_hz).round() as usize;
        let ref_hi = 2.0_f64.min(self.rate_hz / 2.0);
        if self.samples.len() < take || take < 8 || ref_hi <= BREATHING_MAX_HZ {
            return None;
        }
        let start = self.samples.len() - take;
        let fft_len = take.next_power_of_two();
        let res = self.rate_hz / fft_len as f64;
        let ratios: Vec<f64> = ranked
            .iter()
            .take(EVIDENCE_STREAMS)
            .filter_map(|(_, j)| {
                let x = detrend(
                    &self
                        .samples
                        .iter()
                        .skip(start)
                        .map(|s| s[*j])
                        .collect::<Vec<_>>(),
                );
                let mut signal = vec![0.0; fft_len];
                for (i, v) in x.iter().enumerate() {
                    let w = 0.5 * (1.0 - (2.0 * PI * i as f64 / (take as f64 - 1.0)).cos());
                    signal[i] = v * w;
                }
                let power: Vec<f64> = fft_magnitude(&signal).iter().map(|m| m * m).collect();
                let band_peak = power
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| {
                        let f = *k as f64 * res;
                        (BREATHING_MIN_HZ..=BREATHING_MAX_HZ).contains(&f)
                    })
                    .map(|(_, p)| *p)
                    .fold(0.0, f64::max);
                let mut reference: Vec<f64> = power
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| {
                        let f = *k as f64 * res;
                        f > BREATHING_MAX_HZ && f <= ref_hi
                    })
                    .map(|(_, p)| *p)
                    .collect();
                if reference.is_empty() {
                    return None;
                }
                reference.sort_by(f64::total_cmp);
                let median = reference[reference.len() / 2];
                (median > 0.0).then(|| band_peak / median)
            })
            .collect();
        (!ratios.is_empty()).then(|| ratios.iter().sum::<f64>() / ratios.len() as f64)
    }
}

/// One frame's breathing features. Amplitude streams use log-amplitude with
/// the frame mean removed, which cancels per-packet gain changes; the frame
/// mean is kept as one more stream so a signal common to every subcarrier is
/// not lost. Phase streams are added when every subcarrier has a phase.
fn breathing_features(amplitude: &[f64], phase: &[f64]) -> Vec<f64> {
    if amplitude.is_empty() || amplitude.iter().any(|a| !a.is_finite()) {
        return Vec::new();
    }
    let log_amp: Vec<f64> = amplitude.iter().map(|a| a.max(1e-3).ln()).collect();
    let mean = log_amp.iter().sum::<f64>() / log_amp.len() as f64;
    let mut out = Vec::with_capacity(2 * amplitude.len() + 1);
    out.extend(log_amp.iter().map(|v| v - mean));
    out.push(mean);
    if phase.len() == amplitude.len() && phase.len() >= 3 && phase.iter().all(|p| p.is_finite()) {
        out.extend(sanitize_phase(phase));
    }
    out
}

/// Unwrap phase across subcarriers and remove the least-squares line, which
/// cancels the per-packet common phase offset and timing slope that ESP32
/// CSI carries. Subcarriers are assumed to be in frequency order.
fn sanitize_phase(phase: &[f64]) -> Vec<f64> {
    let mut unwrapped = Vec::with_capacity(phase.len());
    let mut offset = 0.0;
    let mut prev = phase[0];
    for &p in phase {
        let d = p - prev;
        if d > PI {
            offset -= 2.0 * PI;
        } else if d < -PI {
            offset += 2.0 * PI;
        }
        unwrapped.push(p + offset);
        prev = p;
    }
    detrend(&unwrapped)
}

/// Remove the least-squares straight line (and so the mean).
fn detrend(x: &[f64]) -> Vec<f64> {
    let n = x.len() as f64;
    if x.len() < 2 {
        return x.iter().map(|_| 0.0).collect();
    }
    let xm = (n - 1.0) / 2.0;
    let ym = x.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, &y) in x.iter().enumerate() {
        let dx = i as f64 - xm;
        num += dx * (y - ym);
        den += dx * dx;
    }
    let slope = if den > 0.0 { num / den } else { 0.0 };
    x.iter()
        .enumerate()
        .map(|(i, &y)| y - ym - slope * (i as f64 - xm))
        .collect()
}

/// Projection of `streams` (each the same length) onto their first
/// principal component, found by power iteration on the covariance.
fn first_principal_component(streams: &[Vec<f64>]) -> Vec<f64> {
    let k = streams.len();
    let n = streams.first().map_or(0, Vec::len);
    if k == 1 {
        return streams[0].clone();
    }
    let mut cov = vec![vec![0.0; k]; k];
    for a in 0..k {
        for b in a..k {
            let c: f64 = streams[a].iter().zip(&streams[b]).map(|(x, y)| x * y).sum();
            cov[a][b] = c;
            cov[b][a] = c;
        }
    }
    let mut v = vec![1.0 / (k as f64).sqrt(); k];
    for _ in 0..100 {
        let mut next: Vec<f64> = (0..k)
            .map(|a| (0..k).map(|b| cov[a][b] * v[b]).sum())
            .collect();
        let norm = next.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm <= f64::EPSILON {
            break;
        }
        next.iter_mut().for_each(|x| *x /= norm);
        let delta: f64 = next.iter().zip(&v).map(|(a, b)| (a - b).abs()).sum();
        v = next;
        if delta < 1e-10 {
            break;
        }
    }
    (0..n)
        .map(|i| (0..k).map(|a| v[a] * streams[a][i]).sum())
        .collect()
}

/// Peak of `x` in [min_hz, max_hz] after a Hann window and `pad`-times zero
/// padding, with parabolic interpolation. Confidence uses the same
/// peak-to-band-mean mapping as [`VitalSignDetector::compute_fft_peak`].
/// With `subharmonic_share`, a peak whose half frequency is in the band and
/// reaches that share of the peak's magnitude is replaced by the half.
fn compute_spectral_peak(
    x: &[f64],
    rate_hz: f64,
    min_hz: f64,
    max_hz: f64,
    pad: usize,
    subharmonic_share: Option<f64>,
) -> (Option<f64>, f64) {
    let n = x.len();
    if n < 4 {
        return (None, 0.0);
    }
    let fft_len = (n * pad.max(1)).next_power_of_two();
    let mut signal = vec![0.0; fft_len];
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * PI * i as f64 / (n as f64 - 1.0)).cos());
        signal[i] = v * w;
    }
    let spectrum = fft_magnitude(&signal);
    let res = rate_hz / fft_len as f64;
    let lo = (min_hz / res).ceil() as usize;
    let hi = ((max_hz / res).floor() as usize).min(spectrum.len().saturating_sub(1));
    if lo >= hi {
        return (None, 0.0);
    }
    let argmax = |from: usize, to: usize| {
        (from..=to)
            .map(|b| (b, spectrum[b]))
            .fold(
                (from, 0.0),
                |best, cur| if cur.1 > best.1 { cur } else { best },
            )
    };
    let (mut peak_bin, mut peak) = argmax(lo, hi);
    if let Some(share) = subharmonic_share {
        // Search half a coarse bin either side of half the peak frequency.
        let half = peak_bin / 2;
        let reach = (pad / 2).max(1);
        if half >= lo + reach {
            let (b, m) = argmax(half - reach, (half + reach).min(hi));
            if m >= share * peak {
                peak_bin = b;
                peak = m;
            }
        }
    }
    let band_mean = spectrum[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64;
    if band_mean <= f64::EPSILON {
        return (None, 0.0);
    }
    let offset = if peak_bin > lo && peak_bin < hi {
        let (a, b, c) = (
            spectrum[peak_bin - 1],
            spectrum[peak_bin],
            spectrum[peak_bin + 1],
        );
        let d = a - 2.0 * b + c;
        if d.abs() > f64::EPSILON {
            0.5 * (a - c) / d
        } else {
            0.0
        }
    } else {
        0.0
    };
    let confidence = peak_ratio_confidence(peak / band_mean);
    let bpm = (peak_bin as f64 + offset) * res * 60.0;
    if confidence > 0.05 {
        (Some(bpm), confidence)
    } else {
        (None, confidence)
    }
}

/// Map a spectral peak-to-band-mean ratio to a 0-1 confidence.
fn peak_ratio_confidence(peak_ratio: f64) -> f64 {
    if peak_ratio >= CONFIDENCE_THRESHOLD {
        ((peak_ratio - 1.0) / (CONFIDENCE_THRESHOLD * 2.0 - 1.0)).clamp(0.0, 1.0)
    } else {
        ((peak_ratio - 1.0) / (CONFIDENCE_THRESHOLD - 1.0) * 0.5).clamp(0.0, 0.5)
    }
}

// ── Bandpass filter ────────────────────────────────────────────────────────

/// Simple FIR bandpass filter using a windowed-sinc design.
///
/// Constructs a bandpass by subtracting two lowpass filters (LPF_high - LPF_low)
/// with a Hamming window. This is a zero-external-dependency implementation
/// suitable for the buffer sizes we encounter (up to ~600 samples).
/// Circular variance of wrapped phase samples in radians.
///
/// Returns `1 - R` where `R` is the mean resultant length of the unit-circle
/// representation of the input. `0.0` means all samples point in the same
/// direction; `1.0` means they are uniformly spread. Stable across the +/-pi
/// discontinuity of `atan2`-derived phases. Returns `0.0` for inputs shorter
/// than 2 samples.
pub(crate) fn phase_circular_variance(phase: &[f64]) -> f64 {
    if phase.len() < 2 {
        return 0.0;
    }
    let (sin_sum, cos_sum) = phase
        .iter()
        .fold((0.0_f64, 0.0_f64), |(s, c), &p| (s + p.sin(), c + p.cos()));
    let n = phase.len() as f64;
    let r = (sin_sum * sin_sum + cos_sum * cos_sum).sqrt() / n;
    (1.0 - r).clamp(0.0, 1.0)
}

pub fn bandpass_filter(data: &[f64], low_hz: f64, high_hz: f64, sample_rate: f64) -> Vec<f64> {
    if data.len() < 3 || sample_rate < f64::EPSILON {
        return data.to_vec();
    }

    // Normalized cutoff frequencies (0 to 0.5)
    let low_norm = low_hz / sample_rate;
    let high_norm = high_hz / sample_rate;

    if low_norm >= high_norm || low_norm >= 0.5 || high_norm <= 0.0 {
        return data.to_vec();
    }

    // FIR filter order: ~3 cycles of the lowest frequency, clamped to [5, 127]
    let filter_order = ((3.0 / low_norm).ceil() as usize).clamp(5, 127);
    // Ensure odd for type-I FIR symmetry
    let filter_order = if filter_order % 2 == 0 {
        filter_order + 1
    } else {
        filter_order
    };

    let half = filter_order / 2;
    let mut coeffs = vec![0.0f64; filter_order];

    // BPF = LPF(high_norm) - LPF(low_norm) with Hamming window
    #[allow(clippy::needless_range_loop)]
    for i in 0..filter_order {
        let n = i as f64 - half as f64;
        let lp_high = if n.abs() < f64::EPSILON {
            2.0 * high_norm
        } else {
            (2.0 * PI * high_norm * n).sin() / (PI * n)
        };
        let lp_low = if n.abs() < f64::EPSILON {
            2.0 * low_norm
        } else {
            (2.0 * PI * low_norm * n).sin() / (PI * n)
        };

        // Hamming window
        let w = 0.54 - 0.46 * (2.0 * PI * i as f64 / (filter_order as f64 - 1.0)).cos();
        coeffs[i] = (lp_high - lp_low) * w;
    }

    // Normalize filter to unit gain at center frequency
    let center_freq = (low_norm + high_norm) / 2.0;
    let gain: f64 = coeffs
        .iter()
        .enumerate()
        .map(|(i, &c)| c * (2.0 * PI * center_freq * i as f64).cos())
        .sum();
    if gain.abs() > f64::EPSILON {
        for c in coeffs.iter_mut() {
            *c /= gain;
        }
    }

    // Apply filter via convolution
    let mut output = vec![0.0f64; data.len()];
    #[allow(clippy::needless_range_loop)]
    for i in 0..data.len() {
        let mut sum = 0.0;
        for (j, &coeff) in coeffs.iter().enumerate() {
            let idx = i as isize - half as isize + j as isize;
            if idx >= 0 && (idx as usize) < data.len() {
                sum += data[idx as usize] * coeff;
            }
        }
        output[i] = sum;
    }

    output
}

// ── FFT implementation ─────────────────────────────────────────────────────

/// Compute the magnitude spectrum of a real-valued signal using radix-2 DIT FFT.
///
/// Input must be power-of-2 length (caller should zero-pad).
/// Returns magnitudes for bins 0..N/2+1.
fn fft_magnitude(signal: &[f64]) -> Vec<f64> {
    let n = signal.len();
    debug_assert!(n.is_power_of_two(), "FFT input must be power-of-2 length");

    if n <= 1 {
        return signal.to_vec();
    }

    // Convert to complex (imaginary = 0)
    let mut real = signal.to_vec();
    let mut imag = vec![0.0f64; n];

    // Bit-reversal permutation
    bit_reverse_permute(&mut real, &mut imag);

    // Cooley-Tukey radix-2 DIT butterfly
    let mut size = 2;
    while size <= n {
        let half = size / 2;
        let angle_step = -2.0 * PI / size as f64;

        for start in (0..n).step_by(size) {
            for k in 0..half {
                let angle = angle_step * k as f64;
                let wr = angle.cos();
                let wi = angle.sin();

                let i = start + k;
                let j = start + k + half;

                let tr = wr * real[j] - wi * imag[j];
                let ti = wr * imag[j] + wi * real[j];

                real[j] = real[i] - tr;
                imag[j] = imag[i] - ti;
                real[i] += tr;
                imag[i] += ti;
            }
        }

        size *= 2;
    }

    // Compute magnitudes for positive frequencies (0..N/2+1)
    let out_len = n / 2 + 1;
    let mut magnitudes = Vec::with_capacity(out_len);
    for i in 0..out_len {
        magnitudes.push((real[i] * real[i] + imag[i] * imag[i]).sqrt());
    }

    magnitudes
}

/// In-place bit-reversal permutation for FFT.
fn bit_reverse_permute(real: &mut [f64], imag: &mut [f64]) {
    let n = real.len();
    let bits = (n as f64).log2() as u32;

    for i in 0..n {
        let j = reverse_bits(i as u32, bits) as usize;
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
    }
}

/// Reverse the lower `bits` bits of `val`.
fn reverse_bits(val: u32, bits: u32) -> u32 {
    let mut result = 0u32;
    let mut v = val;
    for _ in 0..bits {
        result = (result << 1) | (v & 1);
        v >>= 1;
    }
    result
}

// ── Benchmark ──────────────────────────────────────────────────────────────

/// Run a benchmark: process `n_frames` synthetic frames and report timing.
///
/// Generates frames with embedded breathing (0.25 Hz / 15 BPM) and heartbeat
/// (1.2 Hz / 72 BPM) signals on 56 subcarriers at 20 Hz sample rate.
///
/// Returns (total_duration, per_frame_duration).
pub fn run_benchmark(n_frames: usize) -> (std::time::Duration, std::time::Duration) {
    use std::time::Instant;

    let sample_rate = 20.0;
    let mut detector = VitalSignDetector::new(sample_rate);

    // Pre-generate synthetic CSI data (56 subcarriers, matching simulation mode)
    let n_sub = 56;
    let frames: Vec<(Vec<f64>, Vec<f64>)> = (0..n_frames)
        .map(|tick| {
            let t = tick as f64 / sample_rate;
            let mut amp = Vec::with_capacity(n_sub);
            let mut phase = Vec::with_capacity(n_sub);
            for i in 0..n_sub {
                // Embedded breathing at 0.25 Hz (15 BPM) and heartbeat at 1.2 Hz (72 BPM)
                let breathing = 2.0 * (2.0 * PI * 0.25 * t).sin();
                let heartbeat = 0.3 * (2.0 * PI * 1.2 * t).sin();
                let base = 15.0 + 5.0 * (i as f64 * 0.1).sin();
                let noise = (i as f64 * 7.3 + t * 13.7).sin() * 0.5;
                amp.push(base + breathing + heartbeat + noise);
                phase.push((i as f64 * 0.2 + t * 0.5).sin() * PI + heartbeat * 0.1);
            }
            (amp, phase)
        })
        .collect();

    let start = Instant::now();
    let mut last_vital = VitalSigns::default();
    for (amp, phase) in &frames {
        last_vital = detector.process_frame(amp, phase);
    }
    let total = start.elapsed();
    let per_frame = total / n_frames as u32;

    eprintln!("=== Vital Sign Detection Benchmark ===");
    eprintln!("Frames processed:       {}", n_frames);
    eprintln!("Sample rate:            {} Hz", sample_rate);
    eprintln!("Subcarriers:            {}", n_sub);
    eprintln!("Total time:             {:?}", total);
    eprintln!("Per-frame time:         {:?}", per_frame);
    eprintln!(
        "Throughput:             {:.0} frames/sec",
        n_frames as f64 / total.as_secs_f64()
    );
    eprintln!();
    eprintln!("Final vital signs:");
    eprintln!(
        "  Breathing rate:       {:?} BPM",
        last_vital.breathing_rate_bpm
    );
    eprintln!(
        "  Heart rate:           {:?} BPM",
        last_vital.heart_rate_bpm
    );
    eprintln!(
        "  Breathing confidence: {:.3}",
        last_vital.breathing_confidence
    );
    eprintln!(
        "  Heartbeat confidence: {:.3}",
        last_vital.heartbeat_confidence
    );
    eprintln!("  Signal quality:       {:.3}", last_vital.signal_quality);

    (total, per_frame)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the linear-vs-circular phase variance bug.
    ///
    /// Two CSI subcarriers whose phases land just either side of the +/-pi
    /// wrap are physically ~2*eps rad apart, but the previous code computed
    /// arithmetic mean + arithmetic variance on the wrapped values, treating
    /// them as ~2*pi apart and producing variance ~pi^2 ~= 9.87. The corrected
    /// circular variance returns ~1e-6 for the same input.
    #[test]
    fn test_phase_variance_handles_wraparound() {
        // Two phases ~0.002 rad apart, straddling the +/-pi discontinuity.
        let phases = [PI - 0.001, -PI + 0.001];

        let v = phase_circular_variance(&phases);
        assert!(
            v < 0.01,
            "circular variance of nearly-identical wrapped phases must be tiny, got {v}"
        );

        // For reference, the *old* (buggy) linear formula on the same input
        // produced ~9.87. Document that gap here so the assertion above
        // explicitly fails on any regression to the linear computation.
        let mean_linear: f64 = phases.iter().sum::<f64>() / phases.len() as f64;
        let v_linear_buggy: f64 = phases
            .iter()
            .map(|p| (p - mean_linear).powi(2))
            .sum::<f64>()
            / phases.len() as f64;
        assert!(
            v_linear_buggy > 9.0,
            "sanity: linear formula on wrapped input should be ~pi^2, got {v_linear_buggy}"
        );
    }

    /// End-to-end: `process_frame` must not blow up `heartbeat_buffer` when
    /// the input phases happen to straddle the +/-pi wrap. Anyone can run this
    /// against `main` (with the inline linear-formula bug) and observe the
    /// stored value of ~9.86; with the fix it is ~1e-6.
    #[test]
    fn test_process_frame_handles_wrapped_phases() {
        let mut detector = VitalSignDetector::new(20.0);
        let amp = vec![1.0_f64; 8];
        // 8 subcarriers, all physically ~aligned at ~+/-pi, alternating sign.
        let phase = vec![
            PI - 0.001,
            -PI + 0.001,
            PI - 0.001,
            -PI + 0.001,
            PI - 0.001,
            -PI + 0.001,
            PI - 0.001,
            -PI + 0.001,
        ];

        detector.process_frame(&amp, &phase);

        let stored = *detector
            .heartbeat_buffer
            .back()
            .expect("process_frame should push exactly one phase_var");

        assert!(
            stored < 0.1,
            "phase_var pushed into heartbeat_buffer should be near 0 for \
             physically-aligned wrapped phases, got {stored}. The linear \
             (buggy) formula produces ~pi^2 = 9.87 here; the circular \
             (fixed) formula produces ~1e-6."
        );
    }

    /// Diametrically opposite phases must yield maximum circular variance.
    #[test]
    fn test_phase_variance_opposite_phases() {
        let v = phase_circular_variance(&[0.0, PI]);
        assert!(
            v > 0.99,
            "two opposite phases must give circular variance near 1.0, got {v}"
        );
    }

    /// Identical phases must yield zero circular variance, even far from zero.
    #[test]
    fn test_phase_variance_identical_phases() {
        let v = phase_circular_variance(&[2.5, 2.5, 2.5, 2.5]);
        assert!(
            v < 1e-9,
            "identical phases must give circular variance ~0, got {v}"
        );
    }

    #[test]
    fn test_fft_magnitude_dc() {
        let signal = vec![1.0; 8];
        let mag = fft_magnitude(&signal);
        // DC bin should be 8.0 (sum), all others near zero
        assert!((mag[0] - 8.0).abs() < 1e-10);
        for m in &mag[1..] {
            assert!(*m < 1e-10, "non-DC bin should be near zero, got {m}");
        }
    }

    #[test]
    fn test_fft_magnitude_sine() {
        // 16-point signal with a single sinusoid at bin 2
        let n = 16;
        let signal: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * 2.0 * i as f64 / n as f64).sin())
            .collect();
        let mag = fft_magnitude(&signal);
        // Peak should be at bin 2
        let peak_bin = mag
            .iter()
            .enumerate()
            .skip(1) // skip DC
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak_bin, 2);
    }

    #[test]
    fn test_bit_reverse() {
        assert_eq!(reverse_bits(0b000, 3), 0b000);
        assert_eq!(reverse_bits(0b001, 3), 0b100);
        assert_eq!(reverse_bits(0b110, 3), 0b011);
    }

    #[test]
    fn test_bandpass_filter_passthrough() {
        // A sine at the center of the passband should mostly pass through
        let sr = 20.0;
        let freq = 0.25; // center of breathing band
        let n = 200;
        let data: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * freq * i as f64 / sr).sin())
            .collect();
        let filtered = bandpass_filter(&data, 0.1, 0.5, sr);
        // Check that the filtered signal has significant energy
        let energy: f64 = filtered.iter().map(|x| x * x).sum::<f64>() / n as f64;
        assert!(
            energy > 0.01,
            "passband signal should pass through, energy={energy}"
        );
    }

    #[test]
    fn test_bandpass_filter_rejects_out_of_band() {
        // A sine well outside the passband should be attenuated
        let sr = 20.0;
        let freq = 5.0; // way above breathing band
        let n = 200;
        let data: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * freq * i as f64 / sr).sin())
            .collect();
        let in_energy: f64 = data.iter().map(|x| x * x).sum::<f64>() / n as f64;
        let filtered = bandpass_filter(&data, 0.1, 0.5, sr);
        let out_energy: f64 = filtered.iter().map(|x| x * x).sum::<f64>() / n as f64;
        let attenuation = out_energy / in_energy;
        assert!(
            attenuation < 0.3,
            "out-of-band signal should be attenuated, ratio={attenuation}"
        );
    }

    #[test]
    fn test_vital_sign_detector_breathing() {
        let sr = 20.0;
        let mut detector = VitalSignDetector::new(sr);
        let target_bpm = 15.0; // 0.25 Hz
        let target_hz = target_bpm / 60.0;

        // Feed 30 seconds of data with a clear breathing signal
        let n_frames = (sr * 30.0) as usize;
        let mut vitals = VitalSigns::default();
        for frame in 0..n_frames {
            let t = frame as f64 / sr;
            let amp: Vec<f64> = (0..56)
                .map(|i| {
                    let base = 15.0 + 5.0 * (i as f64 * 0.1).sin();
                    let breathing = 3.0 * (2.0 * PI * target_hz * t).sin();
                    base + breathing
                })
                .collect();
            let phase: Vec<f64> = (0..56).map(|i| (i as f64 * 0.2).sin()).collect();
            vitals = detector.process_frame(&amp, &phase);
        }

        // After 30s, breathing should be detected
        assert!(
            vitals.breathing_rate_bpm.is_some(),
            "breathing should be detected after 30s"
        );
        if let Some(rate) = vitals.breathing_rate_bpm {
            let error = (rate - target_bpm).abs();
            assert!(
                error < 3.0,
                "breathing rate {rate:.1} BPM should be near {target_bpm} BPM (error={error:.1})"
            );
        }
    }

    #[test]
    fn spectra_refresh_once_per_second_and_match_a_fresh_estimate() {
        let sr = 20.0;
        let mut detector = VitalSignDetector::new(sr);
        let phase: Vec<f64> = (0..56).map(|i| (i as f64 * 0.2).sin()).collect();
        let amp_at = |frame: usize| -> Vec<f64> {
            let t = frame as f64 / sr;
            (0..56)
                .map(|i| 15.0 + 5.0 * (i as f64 * 0.1).sin() + 3.0 * (2.0 * PI * 0.25 * t).sin())
                .collect()
        };
        let mut last = VitalSigns::default();
        let n_frames = sr as usize * 40;
        let mut recomputes = 0;
        for frame in 0..n_frames {
            last = detector.process_frame(&amp_at(frame), &phase);
            // The counter is reset to 0 only on frames that recompute.
            if detector.frames_since_estimate == 0 {
                recomputes += 1;
            }
        }
        assert_eq!(detector.estimate_interval_frames, 20);
        // Frame 0 (no cached estimate yet), then every 20th frame: 40 spectra
        // in 800 frames. Recomputing on every frame would count 800.
        assert_eq!(recomputes, n_frames / 20);
        // Whatever frame we stopped on, the published estimate is at most
        // one second old and equals what the buffers held at that time.
        let (br_now, _) = detector.extract_breathing();
        let br_cached = last.breathing_rate_bpm.expect("breathing after 40 s");
        let br_now = br_now.expect("breathing from the current buffer");
        assert!((br_cached - br_now).abs() < 0.5, "cached {br_cached} vs fresh {br_now}");
        assert!((br_cached - 15.0).abs() < 1.5);
        // A sample-rate change resets the cache and the interval.
        assert!(detector.reconfigure_sample_rate(50.0));
        assert_eq!(detector.estimate_interval_frames, 50);
        assert!(detector.last_estimate.is_none());
    }

    #[test]
    fn test_vital_sign_detector_reset() {
        let mut detector = VitalSignDetector::new(20.0);
        let amp = vec![10.0; 56];
        let phase = vec![0.0; 56];
        for _ in 0..100 {
            detector.process_frame(&amp, &phase);
        }
        let (br_len, _, hb_len, _) = detector.buffer_status();
        assert!(br_len > 0);
        assert!(hb_len > 0);

        detector.reset();
        let (br_len, _, hb_len, _) = detector.buffer_status();
        assert_eq!(br_len, 0);
        assert_eq!(hb_len, 0);
    }

    #[test]
    fn measured_clock_reconfigures_once_and_clears_mixed_history() {
        let mut detector = VitalSignDetector::new(20.0);
        let amp = vec![10.0; 56];
        let phase = vec![0.0; 56];
        for _ in 0..32 {
            detector.process_frame(&amp, &phase);
        }
        assert!(!detector.reconfigure_sample_rate(18.0));
        assert_eq!(detector.sample_rate_hz(), 20.0);
        assert!(detector.reconfigure_sample_rate(12.0));
        assert_eq!(detector.sample_rate_hz(), 12.0);
        assert_eq!(detector.buffer_status(), (0, 360, 0, 180));
        assert!(!detector.reconfigure_sample_rate(f64::NAN));
    }

    #[test]
    fn test_vital_signs_default() {
        let vs = VitalSigns::default();
        assert!(vs.breathing_rate_bpm.is_none());
        assert!(vs.heart_rate_bpm.is_none());
        assert_eq!(vs.breathing_confidence, 0.0);
        assert_eq!(vs.heartbeat_confidence, 0.0);
        assert_eq!(vs.signal_quality, 0.0);
    }

    #[test]
    fn test_empty_amplitude() {
        let mut detector = VitalSignDetector::new(20.0);
        let vs = detector.process_frame(&[], &[]);
        assert!(vs.breathing_rate_bpm.is_none());
        assert!(vs.heart_rate_bpm.is_none());
    }

    #[test]
    fn test_single_subcarrier() {
        let mut detector = VitalSignDetector::new(20.0);
        // Single subcarrier should not crash
        for i in 0..100 {
            let t = i as f64 / 20.0;
            let amp = vec![10.0 + (2.0 * PI * 0.25 * t).sin()];
            let phase = vec![0.0];
            let _ = detector.process_frame(&amp, &phase);
        }
    }

    /// Deterministic noise in [-1, 1) for the stream tests.
    fn noise(seed: &mut u64) -> f64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    #[test]
    fn sanitize_phase_removes_offset_and_slope_across_the_wrap() {
        // A common offset plus a timing slope, wrapped into (-pi, pi].
        let phase: Vec<f64> = (0..52)
            .map(|i| {
                let p: f64 = 2.9 + 0.35 * i as f64;
                (p + PI).rem_euclid(2.0 * PI) - PI
            })
            .collect();
        let clean = sanitize_phase(&phase);
        assert!(clean.iter().all(|v| v.abs() < 1e-9), "{clean:?}");
    }

    #[test]
    fn breathing_found_when_only_the_common_gain_carries_it() {
        // Each subcarrier is independent noise; breathing moves all of them
        // together, so only the frame-mean stream carries it. Averaging PCA
        // over equally weighted noise streams used to lose this.
        let sr = 20.0;
        let mut det = VitalSignDetector::new(sr);
        let mut seed = 7;
        let target_bpm = 19.5;
        let mut vs = VitalSigns::default();
        for frame in 0..(sr as usize * 40) {
            let t = frame as f64 / sr;
            let gain = 1.0 + 0.05 * (2.0 * PI * target_bpm / 60.0 * t).sin();
            let amp: Vec<f64> = (0..52)
                .map(|i| gain * (20.0 + i as f64 * 0.1) * (1.0 + 0.05 * noise(&mut seed)))
                .collect();
            let phase: Vec<f64> = (0..52).map(|_| 0.2 * noise(&mut seed)).collect();
            vs = det.process_frame(&amp, &phase);
        }
        let bpm = vs.breathing_rate_bpm.expect("breathing estimate");
        assert!((bpm - target_bpm).abs() < 0.5, "{bpm}");
    }

    #[test]
    fn a_strong_second_harmonic_does_not_double_the_rate() {
        let rate = 4.0;
        let n = 120; // 30 s at 4 Hz
        let f0 = 9.9 / 60.0;
        let x: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                0.6 * (2.0 * PI * f0 * t).sin() + (2.0 * PI * 2.0 * f0 * t + 0.4).sin()
            })
            .collect();
        let (with_check, _) =
            compute_spectral_peak(&x, rate, BREATHING_MIN_HZ, BREATHING_MAX_HZ, 4, Some(0.5));
        let (without, _) =
            compute_spectral_peak(&x, rate, BREATHING_MIN_HZ, BREATHING_MAX_HZ, 4, None);
        assert!((without.unwrap() - 19.8).abs() < 0.5, "{without:?}");
        assert!((with_check.unwrap() - 9.9).abs() < 0.5, "{with_check:?}");
    }

    #[test]
    fn a_new_subcarrier_layout_restarts_the_streams() {
        let mut streams = BreathingStreams::new(20.0, 30.0);
        for _ in 0..50 {
            streams.push(&[10.0; 52], &[0.0; 52]);
        }
        assert_eq!(streams.samples.len(), 10);
        assert_eq!(streams.samples[0].len(), 52 + 1 + 52);
        assert!(streams.push(&[10.0; 56], &[0.0; 56]));
        assert!(streams.samples.is_empty());
        assert_eq!(streams.block_frames, 1);
        assert!(!streams.push(&[10.0; 56], &[0.0; 56]));
    }

    #[test]
    fn a_layout_change_restarts_the_reported_history_too() {
        let sr = 20.0;
        let mut det = VitalSignDetector::new(sr);
        let frame = |n: usize, i: usize| -> Vec<f64> {
            let t = i as f64 / sr;
            (0..n)
                .map(|k| 15.0 + (k as f64 * 0.1).sin() + 2.0 * (2.0 * PI * 0.25 * t).sin())
                .collect()
        };
        let mut vs = VitalSigns::default();
        for i in 0..300 {
            vs = det.process_frame(&frame(52, i), &[0.0; 52]);
        }
        assert_eq!(det.buffer_status().0, 300);
        assert!(vs.breathing_rate_bpm.is_some());
        let vs = det.process_frame(&frame(56, 300), &[0.0; 56]);
        assert_eq!(det.buffer_status().0, 1);
        assert!(vs.breathing_rate_bpm.is_none(), "{vs:?}");
    }

    #[test]
    fn breathing_needs_ten_seconds_of_history() {
        let sr = 20.0;
        let mut det = VitalSignDetector::new(sr);
        let amp_at = |frame: usize| -> Vec<f64> {
            let t = frame as f64 / sr;
            (0..56)
                .map(|i| 15.0 + 5.0 * (i as f64 * 0.1).sin() + 3.0 * (2.0 * PI * 0.25 * t).sin())
                .collect()
        };
        let phase = vec![0.0; 56];
        for frame in 0..(sr as usize * 9) {
            det.process_frame(&amp_at(frame), &phase);
        }
        assert_eq!(det.extract_breathing().0, None);
        for frame in (sr as usize * 9)..(sr as usize * 12) {
            det.process_frame(&amp_at(frame), &phase);
        }
        assert!(det.extract_breathing().0.is_some());
    }

    #[test]
    fn test_benchmark_runs() {
        let (total, per_frame) = run_benchmark(100);
        assert!(total.as_nanos() > 0);
        assert!(per_frame.as_nanos() > 0);
    }
}

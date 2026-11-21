//! A choice between the crate's two vitals methods behind one interface.
//!
//! - [`VitalsMethod::Classic`] (the default) is the ADR-021 pipeline that
//!   `ruview-kernel` and the Python bindings have always run:
//!   [`CsiVitalPreprocessor`] residuals, then [`BreathingExtractor`] and
//!   [`HeartRateExtractor`].
//! - [`VitalsMethod::Detector`] is [`VitalSignDetector`], the detector the
//!   sensing server runs.
//!
//! Neither method is removed. The classic method stays the default, so
//! existing users see no change unless they opt in. The scenario harness
//! compares the two (SYNTHETIC); see the crate README.

use std::fmt;
use std::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::breathing::BreathingExtractor;
use crate::detector::{VitalSignDetector, VitalSigns};
use crate::heartrate::HeartRateExtractor;
use crate::preprocessor::CsiVitalPreprocessor;
use crate::types::{CsiFrame, VitalEstimate, VitalStatus};

/// Which vitals method to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum VitalsMethod {
    /// ADR-021 pipeline: EMA residuals, 0.1–0.5 Hz breathing extractor,
    /// 0.8–2.0 Hz heart-rate extractor. The default.
    #[default]
    Classic,
    /// The sensing server's [`VitalSignDetector`]: per-subcarrier breathing
    /// streams combined by PCA, FFT heart rate, spectra once per second.
    Detector,
}

impl VitalsMethod {
    /// Every method, in a stable order (the default first).
    pub const ALL: [VitalsMethod; 2] = [VitalsMethod::Classic, VitalsMethod::Detector];

    /// The method's name as used in configs and APIs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VitalsMethod::Classic => "classic",
            VitalsMethod::Detector => "detector",
        }
    }
}

impl fmt::Display for VitalsMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for VitalsMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "classic" => Ok(VitalsMethod::Classic),
            "detector" => Ok(VitalsMethod::Detector),
            other => Err(format!(
                "unknown vitals method {other:?}; expected \"classic\" or \"detector\""
            )),
        }
    }
}

/// Settings for a [`VitalsEstimator`]. The window lengths apply to the
/// classic method only; the detector keeps its own fixed windows. `alpha`
/// sets the EMA preprocessor, whose residuals both methods report as
/// [`EstimatorStep::residual_rms`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimatorSettings {
    /// Which method to run.
    pub method: VitalsMethod,
    /// Subcarriers per frame.
    pub n_subcarriers: usize,
    /// Frames per second.
    pub sample_rate_hz: f64,
    /// Classic breathing window, seconds.
    pub breathing_window_secs: f64,
    /// Classic heart-rate window, seconds.
    pub heart_window_secs: f64,
    /// EMA preprocessor smoothing factor.
    pub preprocess_alpha: f64,
}

impl EstimatorSettings {
    /// `ruview-kernel`'s defaults for the given method, subcarriers and rate.
    #[must_use]
    pub fn new(method: VitalsMethod, n_subcarriers: usize, sample_rate_hz: f64) -> Self {
        Self {
            method,
            n_subcarriers,
            sample_rate_hz,
            breathing_window_secs: 30.0,
            heart_window_secs: 15.0,
            preprocess_alpha: 0.05,
        }
    }
}

/// One processed frame.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimatorStep {
    /// Latest respiratory-rate estimate.
    pub respiratory_rate: VitalEstimate,
    /// Latest heart-rate estimate.
    pub heart_rate: VitalEstimate,
    /// Root-mean-square of this frame's EMA residuals (signal units).
    pub residual_rms: f64,
}

/// Map the detector's output onto [`VitalEstimate`]s, using the classic
/// extractors' status thresholds so both methods mean the same thing by
/// `Valid`, `Degraded` and `Unreliable` (breathing: confidence ≥ 0.7 /
/// ≥ 0.4; heart rate: ≥ 0.6 / ≥ 0.3). No rate means `Unavailable`.
#[must_use]
pub fn detector_estimates(v: &VitalSigns) -> (VitalEstimate, VitalEstimate) {
    fn estimate(bpm: Option<f64>, confidence: f64, valid: f64, degraded: f64) -> VitalEstimate {
        match bpm {
            Some(value_bpm) if value_bpm.is_finite() => VitalEstimate {
                value_bpm,
                confidence,
                status: if confidence >= valid {
                    VitalStatus::Valid
                } else if confidence >= degraded {
                    VitalStatus::Degraded
                } else {
                    VitalStatus::Unreliable
                },
            },
            _ => VitalEstimate::unavailable(),
        }
    }
    (
        estimate(v.breathing_rate_bpm, v.breathing_confidence, 0.7, 0.4),
        estimate(v.heart_rate_bpm, v.heartbeat_confidence, 0.6, 0.3),
    )
}

struct ClassicEngine {
    breathing: BreathingExtractor,
    heart: HeartRateExtractor,
    weights: Vec<f64>,
}

enum Engine {
    Classic(Box<ClassicEngine>),
    Detector(Box<VitalSignDetector>),
}

/// Runs the chosen [`VitalsMethod`] frame by frame and keeps the latest
/// estimates, the way `ruview-kernel` sessions do.
pub struct VitalsEstimator {
    settings: EstimatorSettings,
    pre: CsiVitalPreprocessor,
    engine: Engine,
    frames: u64,
    last_rr: VitalEstimate,
    last_hr: VitalEstimate,
}

impl VitalsEstimator {
    /// Build an estimator. Callers validate the settings' ranges.
    #[must_use]
    pub fn new(settings: EstimatorSettings) -> Self {
        let n = settings.n_subcarriers.max(1);
        let engine = match settings.method {
            VitalsMethod::Classic => Engine::Classic(Box::new(ClassicEngine {
                breathing: BreathingExtractor::new(
                    n,
                    settings.sample_rate_hz,
                    settings.breathing_window_secs,
                ),
                heart: HeartRateExtractor::new(
                    n,
                    settings.sample_rate_hz,
                    settings.heart_window_secs,
                ),
                weights: vec![1.0 / n as f64; n],
            })),
            VitalsMethod::Detector => {
                Engine::Detector(Box::new(VitalSignDetector::new(settings.sample_rate_hz)))
            }
        };
        Self {
            pre: CsiVitalPreprocessor::new(n, settings.preprocess_alpha),
            engine,
            settings,
            frames: 0,
            last_rr: VitalEstimate::unavailable(),
            last_hr: VitalEstimate::unavailable(),
        }
    }

    /// The method this estimator runs.
    #[must_use]
    pub fn method(&self) -> VitalsMethod {
        self.settings.method
    }

    /// The settings it was built with.
    #[must_use]
    pub fn settings(&self) -> &EstimatorSettings {
        &self.settings
    }

    /// Frames processed so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Process one frame. `phases` may be `None` (treated as zeros). Returns
    /// `None` only when the preprocessor rejects the frame (no amplitudes).
    ///
    /// The classic path does exactly what `ruview-kernel` sessions did before
    /// this module existed: a classic estimate is replaced only when the
    /// extractor returns one.
    pub fn process(&mut self, amplitudes: &[f64], phases: Option<&[f64]>) -> Option<EstimatorStep> {
        let n = self.settings.n_subcarriers.max(1);
        let frame = CsiFrame {
            amplitudes: amplitudes.to_vec(),
            phases: phases.map_or_else(|| vec![0.0; n], <[f64]>::to_vec),
            n_subcarriers: n,
            sample_index: self.frames,
            sample_rate_hz: self.settings.sample_rate_hz,
        };
        self.frames += 1;
        let residuals = self.pre.process(&frame)?;
        let residual_rms = (residuals.iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();
        match &mut self.engine {
            Engine::Classic(classic) => {
                if let Some(rr) = classic.breathing.extract(&residuals, &classic.weights) {
                    self.last_rr = rr;
                }
                if let Some(hr) = classic.heart.extract(&residuals, &frame.phases) {
                    self.last_hr = hr;
                }
            }
            Engine::Detector(detector) => {
                let signs = detector.process_frame(&frame.amplitudes, &frame.phases);
                (self.last_rr, self.last_hr) = detector_estimates(&signs);
            }
        }
        Some(EstimatorStep {
            respiratory_rate: self.last_rr.clone(),
            heart_rate: self.last_hr.clone(),
            residual_rms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SYNTHETIC: 56 subcarriers at 20 Hz with a 15 BPM breathing ripple.
    fn breathing_frames(seconds: usize, bpm: f64) -> Vec<(Vec<f64>, Vec<f64>)> {
        let fs = 20.0;
        (0..seconds * 20)
            .map(|i| {
                let t = i as f64 / fs;
                let breath = (2.0 * std::f64::consts::PI * bpm / 60.0 * t).sin();
                let amps = (0..56)
                    .map(|k| 10.0 + 0.5 * breath * (1.0 + 0.01 * k as f64))
                    .collect();
                let phases = (0..56).map(|k| 0.02 * k as f64 + 0.05 * breath).collect();
                (amps, phases)
            })
            .collect()
    }

    #[test]
    fn method_names_round_trip() {
        for m in VitalsMethod::ALL {
            assert_eq!(m.as_str().parse::<VitalsMethod>(), Ok(m));
        }
        assert_eq!(VitalsMethod::default(), VitalsMethod::Classic);
        assert!("fft".parse::<VitalsMethod>().is_err());
    }

    #[test]
    fn classic_matches_the_three_stage_pipeline_exactly() {
        let frames = breathing_frames(40, 15.0);
        let mut est = VitalsEstimator::new(EstimatorSettings::new(VitalsMethod::Classic, 56, 20.0));
        let mut pre = CsiVitalPreprocessor::new(56, 0.05);
        let mut br = BreathingExtractor::new(56, 20.0, 30.0);
        let mut hr = HeartRateExtractor::new(56, 20.0, 15.0);
        let w = vec![1.0 / 56.0; 56];
        let (mut rr, mut h) = (VitalEstimate::unavailable(), VitalEstimate::unavailable());
        for (i, (a, p)) in frames.iter().enumerate() {
            let step = est.process(a, Some(p)).expect("frame accepted");
            let f = CsiFrame {
                amplitudes: a.clone(),
                phases: p.clone(),
                n_subcarriers: 56,
                sample_index: i as u64,
                sample_rate_hz: 20.0,
            };
            let res = pre.process(&f).unwrap();
            if let Some(e) = br.extract(&res, &w) {
                rr = e;
            }
            if let Some(e) = hr.extract(&res, &f.phases) {
                h = e;
            }
            assert_eq!(step.respiratory_rate, rr);
            assert_eq!(step.heart_rate, h);
        }
    }

    #[test]
    fn detector_finds_the_breathing_rate() {
        let mut est =
            VitalsEstimator::new(EstimatorSettings::new(VitalsMethod::Detector, 56, 20.0));
        let mut last = None;
        for (a, p) in breathing_frames(40, 15.0) {
            last = est.process(&a, Some(&p));
        }
        let rr = last.unwrap().respiratory_rate;
        assert_ne!(rr.status, VitalStatus::Unavailable);
        assert!((rr.value_bpm - 15.0).abs() < 1.0, "breathing {rr:?}");
    }

    #[test]
    fn detector_mapping_uses_the_classic_thresholds() {
        let signs = |br: Option<f64>, bc: f64, hr: Option<f64>, hc: f64| VitalSigns {
            breathing_rate_bpm: br,
            heart_rate_bpm: hr,
            breathing_confidence: bc,
            heartbeat_confidence: hc,
            ..VitalSigns::default()
        };
        let (b, h) = detector_estimates(&signs(Some(15.0), 0.7, Some(70.0), 0.3));
        assert_eq!(
            (b.status, h.status),
            (VitalStatus::Valid, VitalStatus::Degraded)
        );
        let (b, h) = detector_estimates(&signs(Some(15.0), 0.39, Some(70.0), 0.29));
        assert_eq!(
            (b.status, h.status),
            (VitalStatus::Unreliable, VitalStatus::Unreliable)
        );
        let (b, h) = detector_estimates(&signs(None, 0.9, Some(f64::NAN), 0.9));
        assert_eq!(b, VitalEstimate::unavailable());
        assert_eq!(h, VitalEstimate::unavailable());
    }

    #[test]
    fn empty_frames_are_rejected_without_panicking() {
        for m in VitalsMethod::ALL {
            let mut est = VitalsEstimator::new(EstimatorSettings::new(m, 56, 20.0));
            assert!(est.process(&[], None).is_none());
        }
    }
}

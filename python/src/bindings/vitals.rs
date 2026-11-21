//! ADR-117 P3 — PyO3 bindings for `wifi_densepose_vitals`.
//!
//! Surfaces:
//!
//! - `VitalStatus` enum — clinical-grade / degraded / unreliable / unavailable
//! - `VitalEstimate` — single BPM estimate + confidence + status
//! - `VitalReading` — combined HR + BR + signal quality snapshot
//! - `BreathingExtractor` — bandpass 0.1–0.5 Hz → respiratory rate
//! - `HeartRateExtractor` — bandpass 0.8–2.0 Hz + autocorrelation → HR
//! - `VitalsEstimator` — one interface over a choice of vitals method:
//!   `"classic"` (the three classes above, the default) or `"detector"`
//!   (the sensing server's detector). Plan item 4.1.
//! - `VitalSignDetector` / `VitalSigns` — the sensing server's detector
//!   itself (PCA breathing, FFT heart rate)
//!
//! ## GIL release strategy (per ADR-117 §7 and the Q5 audit on
//! 2026-05-24)
//!
//! `wifi-densepose-vitals` has zero tokio deps and the extract loops
//! are pure-sync DSP. Wrap the `.extract(...)` calls in
//! `py.detach(|| ...)` so Python users can run inference in a
//! tokio-backed web server without GIL contention starving the
//! event loop.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use wifi_densepose_vitals::{
    BreathingExtractor, CsiFrame, CsiVitalPreprocessor, EstimatorSettings, HeartRateExtractor,
    VitalEstimate, VitalReading, VitalSignDetector, VitalSigns, VitalStatus, VitalsEstimator,
    VitalsMethod,
};

// ─── VitalStatus enum ────────────────────────────────────────────────

/// Status of a vital sign measurement.
///
/// Python:
/// ```python
/// from wifi_densepose import VitalStatus
/// VitalStatus.Valid       # clinical-grade
/// VitalStatus.Degraded    # reduced confidence
/// VitalStatus.Unreliable  # single RSSI source / low quality
/// VitalStatus.Unavailable # no measurement possible
/// ```
#[pyclass(from_py_object, eq, eq_int, hash, frozen, name = "VitalStatus")]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyVitalStatus {
    Valid = 0,
    Degraded = 1,
    Unreliable = 2,
    Unavailable = 3,
}

#[pymethods]
impl PyVitalStatus {
    fn __repr__(&self) -> String {
        format!("VitalStatus.{:?}", self.as_rust())
    }
}

impl PyVitalStatus {
    fn as_rust(&self) -> VitalStatus {
        match self {
            Self::Valid => VitalStatus::Valid,
            Self::Degraded => VitalStatus::Degraded,
            Self::Unreliable => VitalStatus::Unreliable,
            Self::Unavailable => VitalStatus::Unavailable,
        }
    }

    fn from_rust(s: VitalStatus) -> Self {
        match s {
            VitalStatus::Valid => Self::Valid,
            VitalStatus::Degraded => Self::Degraded,
            VitalStatus::Unreliable => Self::Unreliable,
            VitalStatus::Unavailable => Self::Unavailable,
        }
    }
}

// ─── VitalEstimate ───────────────────────────────────────────────────

/// A single vital-sign estimate (BPM + confidence + status).
///
/// Python:
/// ```python
/// from wifi_densepose import VitalEstimate, VitalStatus
/// est = VitalEstimate(72.4, confidence=0.9, status=VitalStatus.Valid)
/// print(est.value_bpm, est.confidence, est.status)
/// ```
#[pyclass(from_py_object, frozen, name = "VitalEstimate")]
#[derive(Clone)]
pub struct PyVitalEstimate {
    inner: VitalEstimate,
}

#[pymethods]
impl PyVitalEstimate {
    #[new]
    fn new(value_bpm: f64, confidence: f64, status: PyVitalStatus) -> Self {
        Self {
            inner: VitalEstimate {
                value_bpm,
                confidence,
                status: status.as_rust(),
            },
        }
    }

    #[getter]
    fn value_bpm(&self) -> f64 { self.inner.value_bpm }

    #[getter]
    fn confidence(&self) -> f64 { self.inner.confidence }

    #[getter]
    fn status(&self) -> PyVitalStatus { PyVitalStatus::from_rust(self.inner.status) }

    fn __repr__(&self) -> String {
        format!(
            "VitalEstimate(value_bpm={:.2}, confidence={:.3}, status={:?})",
            self.inner.value_bpm, self.inner.confidence, self.inner.status,
        )
    }
}

impl PyVitalEstimate {
    fn from_rust(e: VitalEstimate) -> Self {
        Self { inner: e }
    }
}

// ─── VitalReading ────────────────────────────────────────────────────

/// Combined HR + BR snapshot from one window of CSI data.
#[pyclass(frozen, name = "VitalReading")]
pub struct PyVitalReading {
    inner: VitalReading,
}

#[pymethods]
impl PyVitalReading {
    #[new]
    fn new(
        respiratory_rate: PyVitalEstimate,
        heart_rate: PyVitalEstimate,
        subcarrier_count: usize,
        signal_quality: f64,
        timestamp_secs: f64,
    ) -> Self {
        Self {
            inner: VitalReading {
                respiratory_rate: respiratory_rate.inner,
                heart_rate: heart_rate.inner,
                subcarrier_count,
                signal_quality,
                timestamp_secs,
            },
        }
    }

    #[getter]
    fn respiratory_rate(&self) -> PyVitalEstimate {
        PyVitalEstimate::from_rust(self.inner.respiratory_rate.clone())
    }

    #[getter]
    fn heart_rate(&self) -> PyVitalEstimate {
        PyVitalEstimate::from_rust(self.inner.heart_rate.clone())
    }

    #[getter]
    fn subcarrier_count(&self) -> usize { self.inner.subcarrier_count }

    #[getter]
    fn signal_quality(&self) -> f64 { self.inner.signal_quality }

    #[getter]
    fn timestamp_secs(&self) -> f64 { self.inner.timestamp_secs }

    fn __repr__(&self) -> String {
        format!(
            "VitalReading(br={:.1}, hr={:.1}, subcarriers={}, quality={:.3})",
            self.inner.respiratory_rate.value_bpm,
            self.inner.heart_rate.value_bpm,
            self.inner.subcarrier_count,
            self.inner.signal_quality,
        )
    }
}

// ─── CsiVitalPreprocessor ────────────────────────────────────────────

/// Stage 1 of the vitals pipeline: EMA static-component suppression.
/// Turns raw per-subcarrier amplitudes into body-modulated residuals,
/// which are the input `BreathingExtractor` / `HeartRateExtractor` expect.
///
/// Python:
/// ```python
/// from wifi_densepose import CsiVitalPreprocessor
///
/// pre = CsiVitalPreprocessor.esp32_default()  # 56 subcarriers, alpha=0.05
/// residuals = pre.process(amplitudes)          # list[float] | None
/// ```
#[pyclass(name = "CsiVitalPreprocessor")]
pub struct PyCsiVitalPreprocessor {
    inner: CsiVitalPreprocessor,
}

#[pymethods]
impl PyCsiVitalPreprocessor {
    /// Construct with explicit parameters. `alpha` is clamped to
    /// `[0.001, 0.999]` by the Rust core.
    #[new]
    #[pyo3(signature = (n_subcarriers, alpha=0.05))]
    fn new(n_subcarriers: usize, alpha: f64) -> Self {
        Self { inner: CsiVitalPreprocessor::new(n_subcarriers, alpha) }
    }

    /// ESP32 defaults: 56 subcarriers, alpha = 0.05.
    #[staticmethod]
    fn esp32_default() -> Self {
        Self { inner: CsiVitalPreprocessor::esp32_default() }
    }

    /// Process one frame of per-subcarrier amplitudes and return the
    /// residual vector (observed minus EMA prediction). The first frame
    /// seeds the EMA and returns zeros. Returns `None` for an empty frame.
    fn process(&mut self, py: Python<'_>, amplitudes: Vec<f64>) -> Option<Vec<f64>> {
        let n = amplitudes.len();
        // Phases are unused by the preprocessor; CsiFrame::new only checks
        // that the lengths agree, so pass a zero vector of matching length.
        let frame = CsiFrame::new(amplitudes, vec![0.0; n], n, 0, 0.0)?;
        py.detach(|| self.inner.process(&frame))
    }

    /// Clear all EMA state.
    fn reset(&mut self) {
        self.inner.reset();
    }

    #[getter]
    fn alpha(&self) -> f64 { self.inner.alpha() }

    #[getter]
    fn n_subcarriers(&self) -> usize { self.inner.n_subcarriers() }

    fn __repr__(&self) -> String {
        format!(
            "CsiVitalPreprocessor(n_subcarriers={}, alpha={})",
            self.inner.n_subcarriers(),
            self.inner.alpha(),
        )
    }
}

// ─── BreathingExtractor ──────────────────────────────────────────────

/// Extracts respiratory rate (6–30 BPM) from per-subcarrier amplitude
/// residuals via 0.1–0.5 Hz bandpass + zero-crossing analysis.
///
/// Python:
/// ```python
/// from wifi_densepose import BreathingExtractor
///
/// br = BreathingExtractor.esp32_default()  # 56 subcarriers, 100 Hz, 30s window
/// # or: BreathingExtractor(n_subcarriers=56, sample_rate=100.0, window_secs=30.0)
///
/// # Feed residuals from your preprocessor (one frame at a time)
/// est = br.extract(residuals=[0.01, -0.02, …], weights=[])  # equal weights
/// if est is not None:
///     print(est.value_bpm, est.confidence)
/// ```
#[pyclass(name = "BreathingExtractor")]
pub struct PyBreathingExtractor {
    inner: BreathingExtractor,
}

#[pymethods]
impl PyBreathingExtractor {
    /// Construct with explicit parameters.
    #[new]
    #[pyo3(signature = (n_subcarriers, sample_rate, window_secs=30.0))]
    fn new(n_subcarriers: usize, sample_rate: f64, window_secs: f64) -> Self {
        Self {
            inner: BreathingExtractor::new(n_subcarriers, sample_rate, window_secs),
        }
    }

    /// ESP32 defaults: 56 subcarriers, 100 Hz, 30-second window.
    #[staticmethod]
    fn esp32_default() -> Self {
        Self { inner: BreathingExtractor::esp32_default() }
    }

    /// Extract respiratory rate from a vector of per-subcarrier
    /// residuals + per-subcarrier weights. GIL is released during the
    /// DSP loop so Python threads can do other work concurrently.
    ///
    /// Returns `None` if insufficient history has been accumulated.
    fn extract(&mut self, py: Python<'_>, residuals: Vec<f64>, weights: Vec<f64>) -> Option<PyVitalEstimate> {
        // GIL release: see ADR-117 §7 and the Q5 tokio audit. The DSP
        // loop is pure sync, no Python objects touched, safe to run
        // without the GIL.
        let est = py.detach(|| self.inner.extract(&residuals, &weights));
        est.map(PyVitalEstimate::from_rust)
    }

    fn __repr__(&self) -> String {
        format!("BreathingExtractor(0.1–0.5 Hz bandpass)")
    }
}

// ─── HeartRateExtractor ──────────────────────────────────────────────

/// Extracts heart rate (40–120 BPM) from per-subcarrier amplitude
/// residuals and per-subcarrier unwrapped phases (radians) via
/// 0.8–2.0 Hz bandpass + autocorrelation peak detection.
///
/// Python:
/// ```python
/// from wifi_densepose import HeartRateExtractor
///
/// hr = HeartRateExtractor.esp32_default()  # 56 subcarriers, 100 Hz, 15s window
///
/// # Feed residuals and matching unwrapped phases from your preprocessor.
/// # Like BreathingExtractor's weights, phases=[] means "no per-subcarrier
/// # coherence information available" and falls back to equal weighting
/// # across all subcarriers -- it does NOT silently drop every frame.
/// est = hr.extract(residuals=[0.01, -0.02, …], phases=[0.0, 0.01, …])
/// if est is not None:
///     print(est.value_bpm, est.confidence)
/// ```
#[pyclass(name = "HeartRateExtractor")]
pub struct PyHeartRateExtractor {
    inner: HeartRateExtractor,
}

#[pymethods]
impl PyHeartRateExtractor {
    /// Construct with explicit parameters.
    #[new]
    #[pyo3(signature = (n_subcarriers, sample_rate, window_secs=15.0))]
    fn new(n_subcarriers: usize, sample_rate: f64, window_secs: f64) -> Self {
        Self {
            inner: HeartRateExtractor::new(n_subcarriers, sample_rate, window_secs),
        }
    }

    /// ESP32 defaults: 56 subcarriers, 100 Hz, 15-second window.
    #[staticmethod]
    fn esp32_default() -> Self {
        Self { inner: HeartRateExtractor::esp32_default() }
    }

    /// Extract heart rate from per-subcarrier residuals and matching
    /// per-subcarrier unwrapped phases (radians). A short or empty `phases`
    /// slice falls back to equal weighting for any subcarrier missing phase
    /// data (issue #1423) -- it does not truncate the number of subcarriers
    /// fused, and does not silently return `None` for every frame. GIL
    /// released during DSP.
    fn extract(
        &mut self,
        py: Python<'_>,
        residuals: Vec<f64>,
        phases: Vec<f64>,
    ) -> Option<PyVitalEstimate> {
        let est = py.detach(|| self.inner.extract(&residuals, &phases));
        est.map(PyVitalEstimate::from_rust)
    }

    fn __repr__(&self) -> String {
        format!("HeartRateExtractor(0.8–2.0 Hz bandpass)")
    }
}

// ─── VitalsEstimator (choice of method) ──────────────────────────────

/// Upper bound on subcarriers per frame, as in `ruview-kernel`.
const MAX_SUBCARRIERS: usize = 4096;

fn check_range(name: &str, v: f64, lo: f64, hi: f64) -> PyResult<()> {
    if !v.is_finite() || v < lo || v > hi {
        return Err(PyValueError::new_err(format!(
            "{name} must be a finite number in [{lo}, {hi}]"
        )));
    }
    Ok(())
}

/// Runs the vitals method you choose, frame by frame, and keeps the latest
/// breathing and heart-rate estimates.
///
/// - `method="classic"` (the default): the ADR-021 pipeline that
///   `CsiVitalPreprocessor`, `BreathingExtractor` and `HeartRateExtractor`
///   implement. Results are the same as wiring those three by hand with
///   uniform weights.
/// - `method="detector"`: the sensing server's `VitalSignDetector`. In the
///   repository's scenario harness (SYNTHETIC, not hardware) its breathing
///   error is far lower; see the README's "Choosing a vitals method".
///
/// The window settings apply to `"classic"` only. Both methods report the
/// same `VitalReading` type, with the classic status thresholds.
///
/// Python:
/// ```python
/// from wifi_densepose import VitalsEstimator
///
/// est = VitalsEstimator(method="detector", n_subcarriers=56, sample_rate=20.0)
/// for amplitudes, phases in frames:          # one CSI frame at a time
///     reading = est.process(amplitudes, phases)
/// print(reading.respiratory_rate.value_bpm, reading.respiratory_rate.status)
/// ```
#[pyclass(name = "VitalsEstimator")]
pub struct PyVitalsEstimator {
    inner: VitalsEstimator,
}

#[pymethods]
impl PyVitalsEstimator {
    #[new]
    #[pyo3(signature = (
        method="classic",
        n_subcarriers=56,
        sample_rate=20.0,
        breathing_window_secs=30.0,
        heart_window_secs=15.0,
        alpha=0.05,
    ))]
    fn new(
        method: &str,
        n_subcarriers: usize,
        sample_rate: f64,
        breathing_window_secs: f64,
        heart_window_secs: f64,
        alpha: f64,
    ) -> PyResult<Self> {
        let method: VitalsMethod = method.parse().map_err(PyValueError::new_err)?;
        if n_subcarriers == 0 || n_subcarriers > MAX_SUBCARRIERS {
            return Err(PyValueError::new_err(format!(
                "n_subcarriers must be in [1, {MAX_SUBCARRIERS}]"
            )));
        }
        check_range("sample_rate", sample_rate, 1.0, 1000.0)?;
        check_range("breathing_window_secs", breathing_window_secs, 10.0, 300.0)?;
        check_range("heart_window_secs", heart_window_secs, 5.0, 120.0)?;
        check_range("alpha", alpha, 0.001, 1.0)?;
        Ok(Self {
            inner: VitalsEstimator::new(EstimatorSettings {
                method,
                n_subcarriers,
                sample_rate_hz: sample_rate,
                breathing_window_secs,
                heart_window_secs,
                preprocess_alpha: alpha,
            }),
        })
    }

    /// The methods you can choose from; the first is the default.
    #[staticmethod]
    fn methods() -> Vec<&'static str> {
        VitalsMethod::ALL.iter().map(|m| m.as_str()).collect()
    }

    /// The method this estimator runs.
    #[getter]
    fn method(&self) -> &'static str { self.inner.method().as_str() }

    /// Frames processed so far.
    #[getter]
    fn frames(&self) -> u64 { self.inner.frames() }

    /// Process one frame. `amplitudes` must have `n_subcarriers` values;
    /// `phases` (radians) may be `None` or have the same length. Returns the
    /// latest `VitalReading`. GIL released during DSP.
    #[pyo3(signature = (amplitudes, phases=None))]
    fn process(
        &mut self,
        py: Python<'_>,
        amplitudes: Vec<f64>,
        phases: Option<Vec<f64>>,
    ) -> PyResult<Option<PyVitalReading>> {
        let settings = *self.inner.settings();
        let n = settings.n_subcarriers;
        if amplitudes.len() != n {
            return Err(PyValueError::new_err(format!(
                "amplitudes must have {n} values (got {})",
                amplitudes.len()
            )));
        }
        if let Some(p) = &phases {
            if p.len() != n {
                return Err(PyValueError::new_err(format!(
                    "phases must be None or have {n} values (got {})",
                    p.len()
                )));
            }
        }
        if amplitudes
            .iter()
            .chain(phases.iter().flatten())
            .any(|v| !v.is_finite())
        {
            return Err(PyValueError::new_err("frame contains a non-finite value"));
        }
        let step = py.detach(|| self.inner.process(&amplitudes, phases.as_deref()));
        let frames = self.inner.frames();
        Ok(step.map(|s| PyVitalReading {
            inner: VitalReading {
                signal_quality: (s.respiratory_rate.confidence + s.heart_rate.confidence) / 2.0,
                respiratory_rate: s.respiratory_rate,
                heart_rate: s.heart_rate,
                subcarrier_count: n,
                timestamp_secs: frames as f64 / settings.sample_rate_hz,
            },
        }))
    }

    fn __repr__(&self) -> String {
        format!(
            "VitalsEstimator(method={:?}, n_subcarriers={}, sample_rate={})",
            self.inner.method().as_str(),
            self.inner.settings().n_subcarriers,
            self.inner.settings().sample_rate_hz,
        )
    }
}

// ─── VitalSignDetector (the server's detector) ───────────────────────

/// Output of `VitalSignDetector.process_frame`.
#[pyclass(frozen, name = "VitalSigns")]
pub struct PyVitalSigns {
    inner: VitalSigns,
}

#[pymethods]
impl PyVitalSigns {
    /// Breathing rate in breaths per minute, or `None` if not detected.
    #[getter]
    fn breathing_rate_bpm(&self) -> Option<f64> { self.inner.breathing_rate_bpm }

    /// Heart rate in beats per minute, or `None` if not detected.
    #[getter]
    fn heart_rate_bpm(&self) -> Option<f64> { self.inner.heart_rate_bpm }

    #[getter]
    fn breathing_confidence(&self) -> f64 { self.inner.breathing_confidence }

    #[getter]
    fn heartbeat_confidence(&self) -> f64 { self.inner.heartbeat_confidence }

    #[getter]
    fn signal_quality(&self) -> f64 { self.inner.signal_quality }

    fn __repr__(&self) -> String {
        format!(
            "VitalSigns(breathing_rate_bpm={:?}, heart_rate_bpm={:?}, quality={:.3})",
            self.inner.breathing_rate_bpm, self.inner.heart_rate_bpm, self.inner.signal_quality,
        )
    }
}

/// The detector the RuView sensing server runs: per-subcarrier breathing
/// streams combined by PCA, FFT heart rate, spectra recomputed once per
/// second. `VitalsEstimator(method="detector")` wraps it with the same
/// output type as the classic method.
#[pyclass(name = "VitalSignDetector")]
pub struct PyVitalSignDetector {
    inner: VitalSignDetector,
}

#[pymethods]
impl PyVitalSignDetector {
    #[new]
    #[pyo3(signature = (sample_rate=20.0))]
    fn new(sample_rate: f64) -> PyResult<Self> {
        check_range("sample_rate", sample_rate, 1.0, 1000.0)?;
        Ok(Self { inner: VitalSignDetector::new(sample_rate) })
    }

    /// Process one frame of amplitudes and phases (radians; same length, or
    /// an empty list for no phase data). GIL released during DSP.
    fn process_frame(
        &mut self,
        py: Python<'_>,
        amplitudes: Vec<f64>,
        phases: Vec<f64>,
    ) -> PyResult<PyVitalSigns> {
        if amplitudes.is_empty() || amplitudes.len() > MAX_SUBCARRIERS {
            return Err(PyValueError::new_err(format!(
                "amplitudes must have 1 to {MAX_SUBCARRIERS} values"
            )));
        }
        if !phases.is_empty() && phases.len() != amplitudes.len() {
            return Err(PyValueError::new_err("phases must be empty or match amplitudes"));
        }
        if amplitudes.iter().chain(&phases).any(|v| !v.is_finite()) {
            return Err(PyValueError::new_err("frame contains a non-finite value"));
        }
        let phases = if phases.is_empty() { vec![0.0; amplitudes.len()] } else { phases };
        let signs = py.detach(|| self.inner.process_frame(&amplitudes, &phases));
        Ok(PyVitalSigns { inner: signs })
    }

    fn __repr__(&self) -> String {
        "VitalSignDetector(PCA breathing, FFT heart rate)".to_string()
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVitalStatus>()?;
    m.add_class::<PyVitalEstimate>()?;
    m.add_class::<PyVitalReading>()?;
    m.add_class::<PyCsiVitalPreprocessor>()?;
    m.add_class::<PyBreathingExtractor>()?;
    m.add_class::<PyHeartRateExtractor>()?;
    m.add_class::<PyVitalsEstimator>()?;
    m.add_class::<PyVitalSignDetector>()?;
    m.add_class::<PyVitalSigns>()?;
    Ok(())
}

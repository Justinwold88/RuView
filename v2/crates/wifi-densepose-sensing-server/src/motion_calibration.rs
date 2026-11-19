//! Empty-room motion calibration and link quality (plan item 1.1).
//!
//! The per-node classifier compares a smoothed motion score with fixed
//! thresholds (0.04 for presence). One constant cannot fit every room, radio
//! and placement: a noisy link reads an empty room as occupied, and a quiet
//! one misses small motion. Calibration records the smoothed score of an
//! empty room for about a minute, puts the presence threshold above what the
//! room produced, and scores the link from 0 to 100 from the same minute.
//!
//! The threshold rule and the score weights are a starting point. They are
//! tuned on SYNTHETIC rooms in the scenario harness and not yet MEASURED on
//! real hardware (plan items 2.4 and 2.5).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Presence threshold on the smoothed motion score when a node has no
/// calibration. Matches the long-standing constant.
pub const DEFAULT_PRESENCE_THRESHOLD: f64 = 0.04;
/// Default length of a calibration run.
pub const DEFAULT_CALIBRATION_SECS: f64 = 60.0;
/// Accepted range for a requested run length.
pub const MIN_CALIBRATION_SECS: f64 = 20.0;
pub const MAX_CALIBRATION_SECS: f64 = 600.0;
/// Default wait before collection starts, so whoever started the run can
/// leave the room.
pub const DEFAULT_DELAY_SECS: f64 = 10.0;
pub const MAX_DELAY_SECS: f64 = 300.0;
/// The threshold sits this many times above the empty room's 99th percentile.
/// Chosen on SYNTHETIC rooms: 1.2 left up to 1.9% false presence on a noisy
/// link, 1.5 cost walking detection on a typical one.
pub const THRESHOLD_MARGIN: f64 = 1.3;
/// Lowest threshold calibration may set. Below this, ordinary drift after
/// calibration would read as presence.
pub const MIN_PRESENCE_THRESHOLD: f64 = 0.015;
/// A calibrated threshold above this (the default `present_moving` cut)
/// marks the link as noisy: only clear motion will register.
pub const NOISY_THRESHOLD: f64 = 0.12;
/// Highest threshold calibration may set: the default `active` cut. A room
/// that needs more either was not empty or the link cannot sense at all.
pub const MAX_PRESENCE_THRESHOLD: f64 = 0.25;
/// Fewest post-warm-up frames a run needs (10 s at 20 Hz).
pub const MIN_CALIBRATION_FRAMES: usize = 200;

/// Motion level for a smoothed score, given the node's presence threshold.
///
/// With the default threshold the cuts are the classic 0.04 / 0.12 / 0.25.
/// A calibrated threshold scales the upper cuts so a noisy link never
/// reports `present_moving` for its own noise.
pub fn classify_level(score: f64, presence_threshold: f64) -> &'static str {
    let t = presence_threshold;
    if score > (6.0 * t).max(0.25) {
        "active"
    } else if score > (3.0 * t).max(0.12) {
        "present_moving"
    } else if score > t {
        "present_still"
    } else {
        "absent"
    }
}

/// One frame's worth of calibration input from a node.
#[derive(Debug, Clone, Copy)]
pub struct CalibrationSample {
    /// The node's smoothed motion score after this frame.
    pub smoothed_motion: f64,
    pub rssi_dbm: f64,
    pub noise_floor_dbm: f64,
    /// The frame's sequence number (wraps at `u32::MAX`).
    pub sequence: u32,
    /// Seconds since the run started.
    pub elapsed_secs: f64,
}

/// Collects samples from one node during a calibration run.
#[derive(Debug, Default, Clone)]
pub struct CalibrationCollector {
    scores: Vec<f64>,
    snr_db: Vec<f64>,
    first_sequence: Option<u32>,
    last_sequence: Option<u32>,
    first_elapsed: Option<f64>,
    last_elapsed: f64,
}

/// Why a run produced no calibration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CalibrationError {
    /// Fewer than [`MIN_CALIBRATION_FRAMES`] frames arrived.
    TooFewFrames { frames: usize },
    /// The room produced more motion than any usable empty room: someone
    /// moved, or the link is too noisy to sense.
    TooMuchMotion { empty_p99: f64 },
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalibrationError::TooFewFrames { frames } => write!(
                f,
                "only {frames} frames arrived; at least {MIN_CALIBRATION_FRAMES} are needed"
            ),
            CalibrationError::TooMuchMotion { empty_p99 } => write!(
                f,
                "motion score reached {empty_p99:.3} (99th percentile) in what should be an empty room; \
                 keep the room empty and retry, and if it fails again move the node or its antenna"
            ),
        }
    }
}

/// Link quality from a calibration run. `score` is 0 to 100; the components
/// are reported so an operator can see which one pulled it down.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkQuality {
    pub score: u8,
    /// Median RSSI minus noise floor, in dB.
    pub snr_db: f64,
    /// Frames per second actually received.
    pub frame_rate_hz: f64,
    /// Share of frames missing from the sequence numbers, in percent.
    pub loss_pct: f64,
    /// Empty-room 99th percentile as a share of the default presence
    /// threshold. Lower leaves more room to detect small motion.
    pub noise_ratio: f64,
}

/// Result of a successful run for one node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionCalibration {
    pub presence_threshold: f64,
    /// The threshold is above [`NOISY_THRESHOLD`]: only clear motion will
    /// register on this link.
    pub noisy: bool,
    pub empty_p50: f64,
    pub empty_p99: f64,
    pub empty_max: f64,
    pub frames: usize,
    pub seconds: f64,
    pub link_quality: LinkQuality,
}

/// Round for storage and display. Rounded values also survive a JSON round
/// trip exactly.
fn round_to(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    (x * scale).round() / scale
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

impl CalibrationCollector {
    pub fn push(&mut self, s: CalibrationSample) {
        if !s.smoothed_motion.is_finite() {
            return;
        }
        self.scores.push(s.smoothed_motion.max(0.0));
        let snr = s.rssi_dbm - s.noise_floor_dbm;
        if snr.is_finite() {
            self.snr_db.push(snr);
        }
        self.first_sequence.get_or_insert(s.sequence);
        self.last_sequence = Some(s.sequence);
        self.first_elapsed.get_or_insert(s.elapsed_secs);
        self.last_elapsed = s.elapsed_secs;
    }

    pub fn frames(&self) -> usize {
        self.scores.len()
    }

    /// Turn the collected samples into a calibration.
    pub fn finish(&self) -> Result<MotionCalibration, CalibrationError> {
        let frames = self.scores.len();
        if frames < MIN_CALIBRATION_FRAMES {
            return Err(CalibrationError::TooFewFrames { frames });
        }
        let mut sorted = self.scores.clone();
        sorted.sort_by(f64::total_cmp);
        let empty_p50 = percentile(&sorted, 0.50);
        let empty_p99 = percentile(&sorted, 0.99);
        let empty_max = *sorted.last().unwrap_or(&0.0);
        let wanted = empty_p99 * THRESHOLD_MARGIN;
        if wanted > MAX_PRESENCE_THRESHOLD {
            return Err(CalibrationError::TooMuchMotion { empty_p99 });
        }
        let presence_threshold = round_to(wanted.max(MIN_PRESENCE_THRESHOLD), 4);

        let seconds = (self.last_elapsed - self.first_elapsed.unwrap_or(0.0)).max(0.0);
        let frame_rate_hz = if seconds > 0.0 {
            (frames - 1) as f64 / seconds
        } else {
            0.0
        };
        let expected = match (self.first_sequence, self.last_sequence) {
            (Some(a), Some(b)) => b.wrapping_sub(a) as u64 + 1,
            _ => frames as u64,
        };
        let loss_pct = if expected > frames as u64 {
            100.0 * (expected - frames as u64) as f64 / expected as f64
        } else {
            0.0
        };
        let mut snr = self.snr_db.clone();
        snr.sort_by(f64::total_cmp);
        let snr_db = percentile(&snr, 0.5);
        let noise_ratio = empty_p99 / DEFAULT_PRESENCE_THRESHOLD;

        Ok(MotionCalibration {
            presence_threshold,
            noisy: presence_threshold > NOISY_THRESHOLD,
            empty_p50: round_to(empty_p50, 4),
            empty_p99: round_to(empty_p99, 4),
            empty_max: round_to(empty_max, 4),
            frames,
            seconds: round_to(seconds, 2),
            link_quality: link_quality(snr_db, frame_rate_hz, loss_pct, noise_ratio),
        })
    }
}

/// Score a link 0 to 100. Weights: SNR 30%, frame rate 20%, loss 20%,
/// empty-room quietness 30%. A heuristic, not yet validated on hardware.
pub fn link_quality(
    snr_db: f64,
    frame_rate_hz: f64,
    loss_pct: f64,
    noise_ratio: f64,
) -> LinkQuality {
    let unit = |x: f64| {
        if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let snr = unit((snr_db - 10.0) / 30.0); // 10 dB -> 0, 40 dB -> 1
    let rate = unit(frame_rate_hz / 20.0); // 20 Hz and up -> 1
    let loss = unit(1.0 - loss_pct / 20.0); // 20% loss -> 0
    let quiet = unit(1.0 - noise_ratio); // noise at the default threshold -> 0
    let score = (100.0 * (0.3 * snr + 0.2 * rate + 0.2 * loss + 0.3 * quiet)).round() as u8;
    LinkQuality {
        score,
        snr_db: round_to(snr_db, 1),
        frame_rate_hz: round_to(frame_rate_hz, 2),
        loss_pct: round_to(loss_pct, 2),
        noise_ratio: round_to(noise_ratio, 3),
    }
}

/// Where a calibration run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Waiting for whoever started the run to leave the room.
    Waiting,
    Collecting,
    /// Collection time is over; [`CalibrationRun::finish`] is due.
    Due,
}

/// One calibration run across every node that sends frames during it.
#[derive(Debug, Clone)]
pub struct CalibrationRun {
    delay_secs: f64,
    duration_secs: f64,
    collectors: BTreeMap<u8, CalibrationCollector>,
}

/// Why a run could not start.
#[derive(Debug, Clone, PartialEq)]
pub enum RunRequestError {
    Duration(f64),
    Delay(f64),
}

impl std::fmt::Display for RunRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunRequestError::Duration(v) => write!(
                f,
                "duration_secs must be between {MIN_CALIBRATION_SECS} and {MAX_CALIBRATION_SECS}, got {v}"
            ),
            RunRequestError::Delay(v) => {
                write!(f, "delay_secs must be between 0 and {MAX_DELAY_SECS}, got {v}")
            }
        }
    }
}

impl CalibrationRun {
    /// `None` means the default for that value.
    pub fn new(
        duration_secs: Option<f64>,
        delay_secs: Option<f64>,
    ) -> Result<Self, RunRequestError> {
        let duration_secs = duration_secs.unwrap_or(DEFAULT_CALIBRATION_SECS);
        let delay_secs = delay_secs.unwrap_or(DEFAULT_DELAY_SECS);
        if !(MIN_CALIBRATION_SECS..=MAX_CALIBRATION_SECS).contains(&duration_secs) {
            return Err(RunRequestError::Duration(duration_secs));
        }
        if !(0.0..=MAX_DELAY_SECS).contains(&delay_secs) {
            return Err(RunRequestError::Delay(delay_secs));
        }
        Ok(Self {
            delay_secs,
            duration_secs,
            collectors: BTreeMap::new(),
        })
    }

    pub fn delay_secs(&self) -> f64 {
        self.delay_secs
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_secs
    }

    /// Phase at `elapsed_secs` since the run was started.
    pub fn phase(&self, elapsed_secs: f64) -> RunPhase {
        if elapsed_secs < self.delay_secs {
            RunPhase::Waiting
        } else if elapsed_secs < self.delay_secs + self.duration_secs {
            RunPhase::Collecting
        } else {
            RunPhase::Due
        }
    }

    pub fn seconds_remaining(&self, elapsed_secs: f64) -> f64 {
        (self.delay_secs + self.duration_secs - elapsed_secs).max(0.0)
    }

    /// Record a sample from `node_id`. `sample.elapsed_secs` is time since the
    /// run started; samples outside the collection window are ignored.
    pub fn observe(&mut self, node_id: u8, sample: CalibrationSample) {
        if self.phase(sample.elapsed_secs) == RunPhase::Collecting {
            self.collectors.entry(node_id).or_default().push(sample);
        }
    }

    /// Frames collected so far, per node.
    pub fn frames(&self) -> BTreeMap<u8, usize> {
        self.collectors
            .iter()
            .map(|(id, c)| (*id, c.frames()))
            .collect()
    }

    /// Result per node that sent at least one frame while collecting.
    pub fn finish(&self) -> BTreeMap<u8, Result<MotionCalibration, CalibrationError>> {
        self.collectors
            .iter()
            .map(|(id, c)| (*id, c.finish()))
            .collect()
    }
}

/// File name under the server's data directory.
pub const STORE_FILE: &str = "motion_calibration.json";
const STORE_VERSION: u32 = 1;
/// The file holds a few hundred bytes per node; anything far larger is not ours.
const MAX_STORE_BYTES: u64 = 1 << 20;

#[derive(Serialize, Deserialize)]
struct Store {
    version: u32,
    nodes: BTreeMap<u8, MotionCalibration>,
}

/// A stored calibration is used only when its threshold is one calibration
/// could have produced.
fn plausible(c: &MotionCalibration) -> bool {
    c.presence_threshold.is_finite()
        && (MIN_PRESENCE_THRESHOLD..=MAX_PRESENCE_THRESHOLD).contains(&c.presence_threshold)
}

/// Load stored calibrations. A missing file is an empty store; an unreadable,
/// oversized, malformed or wrong-version file is reported and ignored, and
/// implausible entries are dropped.
pub fn load(path: &Path) -> Result<BTreeMap<u8, MotionCalibration>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if meta.len() > MAX_STORE_BYTES {
        return Err(format!(
            "{}: {} bytes is too large",
            path.display(),
            meta.len()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let store: Store =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if store.version != STORE_VERSION {
        return Err(format!(
            "{}: unsupported version {}",
            path.display(),
            store.version
        ));
    }
    Ok(store
        .nodes
        .into_iter()
        .filter(|(_, c)| plausible(c))
        .collect())
}

/// Save calibrations, replacing the file atomically.
pub fn save(path: &Path, nodes: &BTreeMap<u8, MotionCalibration>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(&Store {
        version: STORE_VERSION,
        nodes: nodes.clone(),
    })
    .map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(scores: impl Iterator<Item = f64>, step: u32) -> CalibrationCollector {
        let mut c = CalibrationCollector::default();
        for (i, s) in scores.enumerate() {
            c.push(CalibrationSample {
                smoothed_motion: s,
                rssi_dbm: -50.0,
                noise_floor_dbm: -90.0,
                sequence: (i as u32) * step,
                elapsed_secs: i as f64 / 20.0,
            });
        }
        c
    }

    #[test]
    fn default_threshold_keeps_the_classic_cuts() {
        let t = DEFAULT_PRESENCE_THRESHOLD;
        assert_eq!(classify_level(0.03, t), "absent");
        assert_eq!(classify_level(0.05, t), "present_still");
        assert_eq!(classify_level(0.13, t), "present_moving");
        assert_eq!(classify_level(0.26, t), "active");
    }

    #[test]
    fn noisy_link_raises_every_cut() {
        assert_eq!(classify_level(0.10, 0.08), "present_still");
        assert_eq!(classify_level(0.20, 0.08), "present_still");
        assert_eq!(classify_level(0.30, 0.08), "present_moving");
        assert_eq!(classify_level(0.50, 0.08), "active");
    }

    #[test]
    fn threshold_sits_above_the_empty_room() {
        // 1200 frames between 0 and 0.02.
        let c = run((0..1200).map(|i| 0.02 * (i % 100) as f64 / 100.0), 1);
        let cal = c.finish().unwrap();
        assert!(cal.empty_p99 <= 0.02);
        assert!((cal.presence_threshold - cal.empty_p99 * THRESHOLD_MARGIN).abs() < 1e-4);
        assert!(cal.presence_threshold > cal.empty_max);
        assert!((cal.link_quality.frame_rate_hz - 20.0).abs() < 0.1);
        assert_eq!(cal.link_quality.loss_pct, 0.0);
    }

    #[test]
    fn very_quiet_room_is_floored() {
        let cal = run((0..1200).map(|_| 0.0001), 1).finish().unwrap();
        assert_eq!(cal.presence_threshold, MIN_PRESENCE_THRESHOLD);
    }

    #[test]
    fn motion_during_calibration_is_refused() {
        let err = run((0..1200).map(|i| if i % 10 == 0 { 0.3 } else { 0.01 }), 1).finish();
        assert!(
            matches!(err, Err(CalibrationError::TooMuchMotion { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn noisy_link_is_accepted_and_flagged() {
        let cal = run((0..1200).map(|i| 0.08 + 0.02 * (i % 7) as f64 / 7.0), 1)
            .finish()
            .unwrap();
        assert!(cal.noisy);
        assert!(cal.presence_threshold > NOISY_THRESHOLD);
        let quiet = run((0..1200).map(|_| 0.02), 1).finish().unwrap();
        assert!(!quiet.noisy);
    }

    #[test]
    fn run_collects_only_inside_its_window() {
        let mut r = CalibrationRun::new(Some(20.0), Some(5.0)).unwrap();
        for i in 0..600u32 {
            let t = i as f64 / 20.0; // 0..30 s
            r.observe(
                1,
                CalibrationSample {
                    smoothed_motion: 0.01,
                    rssi_dbm: -50.0,
                    noise_floor_dbm: -90.0,
                    sequence: i,
                    elapsed_secs: t,
                },
            );
        }
        assert_eq!(r.phase(1.0), RunPhase::Waiting);
        assert_eq!(r.phase(10.0), RunPhase::Collecting);
        assert_eq!(r.phase(25.0), RunPhase::Due);
        assert_eq!(r.frames()[&1], 400); // 5 s to 25 s at 20 Hz
        assert!(r.finish()[&1].is_ok());
    }

    #[test]
    fn run_request_is_validated() {
        assert!(CalibrationRun::new(None, None).is_ok());
        assert_eq!(
            CalibrationRun::new(Some(5.0), None).unwrap_err(),
            RunRequestError::Duration(5.0)
        );
        assert_eq!(
            CalibrationRun::new(None, Some(-1.0)).unwrap_err(),
            RunRequestError::Delay(-1.0)
        );
        assert!(CalibrationRun::new(Some(f64::NAN), None).is_err());
    }

    #[test]
    fn store_round_trips_and_drops_implausible_entries() {
        let dir = std::env::temp_dir().join(format!("ruview-motion-cal-{}", std::process::id()));
        let path = dir.join(STORE_FILE);
        assert!(
            load(&path).unwrap().is_empty(),
            "missing file is an empty store"
        );
        let good = run((0..1200).map(|_| 0.02), 1).finish().unwrap();
        let mut bad = good.clone();
        bad.presence_threshold = 5.0;
        let nodes = BTreeMap::from([(1u8, good.clone()), (2u8, bad)]);
        save(&path, &nodes).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[&1], good);
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn short_run_is_refused() {
        let err = run((0..50).map(|_| 0.01), 1).finish();
        assert_eq!(err, Err(CalibrationError::TooFewFrames { frames: 50 }));
    }

    #[test]
    fn missing_sequence_numbers_count_as_loss() {
        // Every other frame lost: sequence advances by 2.
        let cal = run((0..1200).map(|_| 0.01), 2).finish().unwrap();
        assert!((cal.link_quality.loss_pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn link_score_rewards_snr_rate_low_loss_and_quiet() {
        let good = link_quality(40.0, 20.0, 0.0, 0.0);
        assert_eq!(good.score, 100);
        let bad = link_quality(5.0, 2.0, 30.0, 1.5);
        assert!(bad.score <= 5, "{}", bad.score);
        let mid = link_quality(25.0, 20.0, 2.0, 0.5);
        assert!(mid.score > bad.score && mid.score < good.score);
    }
}

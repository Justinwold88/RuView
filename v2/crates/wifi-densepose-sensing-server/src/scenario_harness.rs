//! SYNTHETIC scenario harness: plan item 0.1 (review item R1).
//!
//! Generates 39 CSI scenarios from a small physical model and replays each
//! one, frame by frame, through the same per-node ESP32 path the server runs
//! for live frames (`extract_features_from_frame`, `smooth_and_classify_node`,
//! the vital-sign detector and `smooth_vitals_node`). It then scores presence,
//! still-person hold and breathing error against ground truth.
//!
//! **Every accuracy number here is SYNTHETIC.** It measures the code against
//! a simple model of a room, not a real one. Per-frame timings are MEASURED
//! on whatever machine runs the test and are reported, never asserted.
//!
//! The model: a static six-path room; optionally a person (chest motion from
//! breathing and heartbeat, or walking); and the impairments ESP32 CSI is
//! known to carry: a random common phase offset and a random phase slope per
//! packet, per-packet gain jitter, additive noise and int8 I/Q quantization.
//! It has no multipath from the person, no furniture motion, no pets and no
//! interference.
//!
//! Gate G1 asks for results that hold across three random seed sets, so the
//! 39 scenarios also run with seeds offset by [`SEED_SET_STRIDE`] (sets 1
//! and 2; set 0 is the original list). A short G1 summary per set is
//! reported and ratcheted.
//!
//! A further set of runs checks the empty-room calibration (plan item 1.1):
//! empty, walking and walk-in rooms on a typical, a noisy and a clean link,
//! each run once with the default presence threshold and once with the
//! threshold a 60 s calibration of the same empty room produced.
//!
//! The non-human filter (plan item 1.5) is checked with a running fan added
//! to the empty and walk-in-and-sit rooms of seed set 0: a typical box fan
//! (the plan's acceptance case), a slower and a weaker one, and one stronger
//! than the person's reflection, which the filter reports instead of
//! removing.
//!
//! Best-link mode (plan item 1.4) is checked with three links on one room:
//! link 1 sees the person on a typical link, link 2's path never crosses
//! them, and link 3 sees them through a noisy link. A five-minute ranking
//! period picks the link the room follows; the room verdict is then compared
//! with fusing all three.
//!
//! The run is a ratchet, not a target. It fails when a metric is worse than
//! the committed baseline (`tests/fixtures/scenario_baseline.json`) by more
//! than a small tolerance. After an intentional improvement, refresh the
//! baseline and commit it with the change:
//!
//! ```text
//! RUVIEW_SCENARIO_UPDATE_BASELINE=1 cargo test --release \
//!   -p wifi-densepose-sensing-server --no-default-features \
//!   scenario_harness -- --ignored --nocapture
//! ```
//!
//! It is `#[ignore]`d because a debug build takes minutes. CI runs it in
//! release mode from `.github/workflows/scenario-harness.yml`. Set
//! `RUVIEW_SCENARIO_REPORT=<path>` to also write the Markdown report there.

use super::*;
use std::f64::consts::PI;
use std::fmt::Write as _;
use std::time::Instant;

/// Bump when the generator or the scenario list changes, so a stale baseline
/// is refused instead of compared.
const GENERATOR_VERSION: &str = "scenario-gen-1";

/// Seed sets for gate G1. Set 0 is the original scenario list.
const SEED_SETS: u64 = 3;
/// Added to every seed per seed set, far above any seed in set 0.
const SEED_SET_STRIDE: u64 = 10_000;

/// Gate G1 limits (plan of action, Phase 1).
const G1_BREATH_MAE_BPM: f64 = 0.5;
const G1_FRAME_US: f64 = 264.0;

/// A room "reads occupied" when presence is reported for more than this share
/// of the empty, post-warm-up frames.
const OCCUPIED_SHARE_PCT: f64 = 5.0;
/// A still person counts as "held" when presence stays on for at least this
/// share of the 100-240 s window.
const HELD_SHARE_PCT: f64 = 90.0;

/// Ratchet tolerances. The generator and the code are deterministic, so
/// these only absorb floating-point differences between platforms.
const TOL_PCT: f64 = 2.0;
const TOL_BPM: f64 = 0.25;

/// Link conditions for the calibration runs. `typical` is the scenario
/// default used everywhere else.
struct Link {
    name: &'static str,
    snr_db: f64,
    agc_db: f64,
    sto_ns: f64,
}

const LINKS: [Link; 3] = [
    Link {
        name: "typical",
        snr_db: 25.0,
        agc_db: 0.5,
        sto_ns: 25.0,
    },
    Link {
        name: "noisy",
        snr_db: 12.0,
        agc_db: 1.5,
        sto_ns: 40.0,
    },
    Link {
        name: "clean",
        snr_db: 35.0,
        agc_db: 0.2,
        sto_ns: 10.0,
    },
];

/// XORed into a scenario's seed to give its calibration segment fresh noise.
const CALIBRATION_STREAM: u64 = 0xCA1B_0000_0000_0001;
/// When the person enters in enter-then-sit scenarios, and how long they walk.
const T_ENTER_SECS: f64 = 30.0;
const T_SIT_SECS: f64 = 40.0;
/// G2's walk-in target: presence within 2 s of entering.
const WALK_IN_SECS: f64 = 2.0;

/// Reported noise floor; RSSI is set from the link's SNR.
const NOISE_FLOOR_DBM: f64 = -90.0;

const C_LIGHT: f64 = 299_792_458.0;
const FC_HZ: f64 = 2.437e9; // channel 6
const DF_HZ: f64 = 312.5e3; // HT20 subcarrier spacing

// ---------------------------------------------------------------------------
// Deterministic random numbers (SplitMix64 + Box-Muller). No external crate,
// so the scenario set cannot drift when a dependency changes its algorithm.
// ---------------------------------------------------------------------------

struct Rng {
    state: u64,
    spare: Option<f64>,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x5DEE_CE66_D1CE_4E5B,
            spare: None,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    /// Standard normal.
    fn normal(&mut self) -> f64 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let u1 = 1.0 - self.uniform(); // (0, 1]
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        self.spare = Some(r * (2.0 * PI * u2).sin());
        r * (2.0 * PI * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Person {
    None,
    Still,
    Walk,
    EnterThenStill,
}

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Empty,
    Walk,
    EnterStill,
    Vitals,
}

#[derive(Clone)]
struct Scenario {
    name: String,
    family: Family,
    seconds: f64,
    fs: f64,
    seed: u64,
    person: Person,
    /// Person reflection power relative to the static channel, in dB.
    dyn_db: f64,
    breath_bpm: f64,
    heart_bpm: f64,
    /// Peak chest displacement from breathing, in mm.
    breath_mm: f64,
    /// Peak chest displacement from the heartbeat, in mm.
    heart_mm: f64,
    /// Standard deviation of the packet-detection delay, in ns.
    sto_ns: f64,
    /// Standard deviation of the per-packet gain jitter, in dB.
    agc_db: f64,
    snr_db: f64,
    /// A running fan (plan item 1.5): a reflection whose strength the
    /// blades chop at a fixed rate. `None` in the 39 standard scenarios.
    fan: Option<Fan>,
}

/// A fan's blades chop its reflection on and off at the blade-pass rate.
#[derive(Clone, Copy)]
struct Fan {
    /// Reflection power relative to the static channel, in dB.
    db: f64,
    /// Blade-pass rate, Hz.
    hz: f64,
}

impl Scenario {
    fn new(name: String, family: Family, seed: u64, person: Person) -> Self {
        Self {
            name,
            family,
            seconds: 120.0,
            fs: 20.0,
            seed,
            person,
            dyn_db: -15.0,
            breath_bpm: 15.0,
            heart_bpm: 72.0,
            breath_mm: 2.5,
            heart_mm: 0.25,
            sto_ns: 25.0,
            agc_db: 0.5,
            snr_db: 25.0,
            fan: None,
        }
    }
}

/// The 39 scenarios from the review: 5 empty rooms, 5 walking, 5 that enter,
/// walk and then sit still, and 24 still people across three reflection
/// strengths for vital signs.
fn scenarios() -> Vec<Scenario> {
    scenarios_for(0)
}

/// The same 39 scenarios with every seed offset by `set` strides: new rooms,
/// new noise and new breathing and heart rates. Set 0 is [`scenarios`].
fn scenarios_for(set: u64) -> Vec<Scenario> {
    let offset = set * SEED_SET_STRIDE;
    let mut out = Vec::new();
    for s in 0..5u64 {
        out.push(Scenario::new(
            format!("empty_s{s}"),
            Family::Empty,
            offset + s,
            Person::None,
        ));
    }
    for s in 0..5u64 {
        let mut sc = Scenario::new(
            format!("walk_s{s}"),
            Family::Walk,
            offset + 100 + s,
            Person::Walk,
        );
        sc.dyn_db = -12.0;
        out.push(sc);
    }
    for s in 0..5u64 {
        let mut sc = Scenario::new(
            format!("enter_still_s{s}"),
            Family::EnterStill,
            offset + 200 + s,
            Person::EnterThenStill,
        );
        sc.seconds = 240.0;
        out.push(sc);
    }
    let mut pick = Rng::new(offset + 7);
    let mut i = 0u64;
    for dyn_db in [-10.0, -15.0, -20.0] {
        for _ in 0..8 {
            let mut sc = Scenario::new(
                format!("vitals_{i:02}"),
                Family::Vitals,
                offset + 300 + i,
                Person::Still,
            );
            sc.seconds = 90.0;
            sc.dyn_db = dyn_db;
            sc.breath_bpm = (pick.range(8.0, 24.0) * 10.0).round() / 10.0;
            sc.heart_bpm = (pick.range(55.0, 100.0) * 10.0).round() / 10.0;
            out.push(sc);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Generator
// ---------------------------------------------------------------------------

struct Generated {
    amplitudes: Vec<Vec<f64>>,
    phases: Vec<Vec<f64>>,
    present: Vec<bool>,
}

/// Multiplies a complex number by e^{j*theta}.
fn rotate(re: f64, im: f64, theta: f64) -> (f64, f64) {
    let (s, c) = theta.sin_cos();
    (re * c - im * s, re * s + im * c)
}

fn generate(sc: &Scenario) -> Generated {
    generate_with(sc, None)
}

/// `impairment_seed` replaces the random stream for everything after the
/// room and the person (per-packet impairments and noise). The room itself
/// still comes from `sc.seed`, so a calibration segment can record the same
/// room with fresh noise.
fn generate_with(sc: &Scenario, impairment_seed: Option<u64>) -> Generated {
    let mut rng = Rng::new(sc.seed);
    let k_idx: Vec<f64> = (-26..=26).filter(|k| *k != 0).map(|k| k as f64).collect();
    let fk: Vec<f64> = k_idx.iter().map(|k| FC_HZ + k * DF_HZ).collect();
    let n = (sc.seconds * sc.fs).round() as usize;

    // Static room: six paths with exponentially decaying power.
    let mut delays: Vec<f64> = (0..6).map(|_| rng.range(10e-9, 80e-9)).collect();
    delays.sort_by(f64::total_cmp);
    let gains: Vec<(f64, f64)> = delays
        .iter()
        .map(|d| {
            let s = ((-d / 30e-9).exp() / 2.0).sqrt();
            (s * rng.normal(), s * rng.normal())
        })
        .collect();
    let hs: Vec<(f64, f64)> = fk
        .iter()
        .map(|f| {
            delays.iter().zip(&gains).fold((0.0, 0.0), |acc, (d, g)| {
                let (re, im) = rotate(g.0, g.1, -2.0 * PI * f * d);
                (acc.0 + re, acc.1 + im)
            })
        })
        .collect();
    let p_static = hs.iter().map(|(re, im)| re * re + im * im).sum::<f64>() / hs.len() as f64;
    let a_dyn = (p_static * 10f64.powf(sc.dyn_db / 10.0)).sqrt();

    let phi_b = rng.range(0.0, 2.0 * PI);
    let phi_h = rng.range(0.0, 2.0 * PI);
    // Slow sub-millimetre drift of a still body.
    let mut sway = Vec::with_capacity(n);
    let mut acc = 0.0;
    for _ in 0..n {
        acc += rng.normal();
        sway.push(2.0 * 0.4e-3 * acc / (n as f64).sqrt());
    }

    let mut rng = match impairment_seed {
        Some(seed) => Rng::new(seed),
        None => rng,
    };

    let d0 = 4.0; // reflection path length, m
    let (t_enter, t_sit) = (T_ENTER_SECS, T_SIT_SECS);
    let sit_index = (t_sit * sc.fs).round() as usize;
    let walk_path = |t: f64| d0 + 1.5 * (0.8 / 1.5 * t).sin();
    let sit_anchor = walk_path(sit_index as f64 / sc.fs - t_enter);

    let noise_std = (p_static * 10f64.powf(-sc.snr_db / 10.0) / 2.0).sqrt();
    let scale = 20.0 / p_static.sqrt();
    let quantize = |x: f64| (x * scale).round_ties_even().clamp(-128.0, 127.0);

    let mut out = Generated {
        amplitudes: Vec::with_capacity(n),
        phases: Vec::with_capacity(n),
        present: Vec::with_capacity(n),
    };
    for i in 0..n {
        let t = i as f64 / sc.fs;
        let chest = 2.0 * sc.breath_mm * 1e-3 * (2.0 * PI * sc.breath_bpm / 60.0 * t + phi_b).sin()
            + 2.0 * sc.heart_mm * 1e-3 * (2.0 * PI * sc.heart_bpm / 60.0 * t + phi_h).sin();
        let (present, moving, path) = match sc.person {
            Person::None => (false, false, 0.0),
            Person::Still => (true, false, d0 + chest + sway[i]),
            Person::Walk => (true, true, walk_path(t) + chest),
            Person::EnterThenStill => {
                if t < t_enter {
                    (false, false, 0.0)
                } else if i < sit_index {
                    (true, true, walk_path(t - t_enter))
                } else {
                    (true, false, sit_anchor + chest + sway[i])
                }
            }
        };
        let flutter = if moving {
            1.0 + 0.3 * (2.0 * PI * 0.7 * t + 1.0).sin()
        } else {
            1.0
        };

        // Per-packet impairments.
        let cpo = rng.range(-PI, PI);
        let sto = rng.normal() * sc.sto_ns * 1e-9;
        let agc = 10f64.powf(rng.normal() * sc.agc_db / 20.0);

        let mut amps = Vec::with_capacity(fk.len());
        let mut phases = Vec::with_capacity(fk.len());
        for (j, f) in fk.iter().enumerate() {
            let (mut re, mut im) = hs[j];
            if present {
                let (pr, pi) = rotate(a_dyn * flutter, 0.0, -2.0 * PI * f * path / C_LIGHT);
                re += pr;
                im += pi;
            }
            if let Some(fan) = sc.fan {
                // Fixed 3 m path; no random draws, so the other scenarios'
                // random streams are untouched.
                let a_fan = (p_static * 10f64.powf(fan.db / 10.0)).sqrt();
                let chop = 0.5 + 0.5 * (2.0 * PI * fan.hz * t + 0.3).sin();
                let (fr, fi) = rotate(a_fan * chop, 0.0, -2.0 * PI * f * 3.0 / C_LIGHT);
                re += fr;
                im += fi;
            }
            let (rr, ri) = rotate(agc * re, agc * im, cpo - 2.0 * PI * k_idx[j] * DF_HZ * sto);
            let qr = quantize(rr + noise_std * rng.normal());
            let qi = quantize(ri + noise_std * rng.normal());
            amps.push(qr.hypot(qi));
            phases.push(qi.atan2(qr));
        }
        out.amplitudes.push(amps);
        out.phases.push(phases);
        out.present.push(present);
    }
    out
}

// ---------------------------------------------------------------------------
// Replay through the server's per-node path
// ---------------------------------------------------------------------------

struct FrameOut {
    presence: bool,
    /// The node's debounced motion level and confidence, for room fusion.
    level: String,
    confidence: f64,
    smoothed_motion: f64,
    detector_br: Option<f64>,
    detector_hr: Option<f64>,
}

#[derive(Default)]
struct Timing {
    frames: u64,
    features_us: f64,
    vitals_us: f64,
    total_us: f64,
}

fn frame_from(amplitudes: Vec<f64>, phases: Vec<f64>, sequence: u32) -> Esp32Frame {
    Esp32Frame {
        magic: 0xC511_0001,
        node_id: 1,
        n_antennas: 1,
        n_subcarriers: amplitudes.len() as u16,
        freq_mhz: 2437,
        sequence,
        rssi: -50,
        noise_floor: NOISE_FLOOR_DBM as i8,
        ppdu_type: wifi_densepose_hardware::PpduType::HtLegacy,
        adr018_flags: wifi_densepose_hardware::Adr018Flags::default(),
        amplitudes,
        phases,
    }
}

fn replay(sc: &Scenario, g: &Generated, timing: &mut Timing) -> Vec<FrameOut> {
    replay_with_threshold(
        sc,
        g,
        timing,
        motion_calibration::DEFAULT_PRESENCE_THRESHOLD,
    )
}

fn replay_with_threshold(
    sc: &Scenario,
    g: &Generated,
    timing: &mut Timing,
    presence_threshold: f64,
) -> Vec<FrameOut> {
    let mut ns = NodeState::new();
    ns.presence_threshold = presence_threshold;
    ns.csi_fps_ema = sc.fs;
    ns.vital_detector = VitalSignDetector::new(sc.fs);
    let mut out = Vec::with_capacity(g.amplitudes.len());
    for (i, (a, p)) in g.amplitudes.iter().zip(&g.phases).enumerate() {
        let frame = frame_from(a.clone(), p.clone(), i as u32);
        let t_all = Instant::now();
        ns.frame_history.push_back(frame.amplitudes.clone());
        if ns.frame_history.len() > FRAME_HISTORY_CAPACITY {
            ns.frame_history.pop_front();
        }
        let t0 = Instant::now();
        let (_features, mut cls, _sub_var, raw_motion) =
            extract_features_from_frame(&frame, &ns.frame_history, sc.fs);
        smooth_and_classify_node(&mut ns, &mut cls, raw_motion);
        let features_us = t0.elapsed().as_secs_f64() * 1e6;
        let t1 = Instant::now();
        let raw = ns
            .vital_detector
            .process_frame(&frame.amplitudes, &frame.phases);
        let _published = smooth_vitals_node(&mut ns, &raw);
        let vitals_us = t1.elapsed().as_secs_f64() * 1e6;
        let _persons = estimate_persons_from_correlation(&ns.frame_history);
        timing.frames += 1;
        timing.features_us += features_us;
        timing.vitals_us += vitals_us;
        timing.total_us += t_all.elapsed().as_secs_f64() * 1e6;
        out.push(FrameOut {
            presence: cls.presence,
            level: cls.motion_level.clone(),
            confidence: cls.confidence,
            smoothed_motion: ns.smoothed_motion,
            detector_br: raw.breathing_rate_bpm,
            detector_hr: raw.heart_rate_bpm,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Empty-room calibration (plan item 1.1)
// ---------------------------------------------------------------------------

/// Calibrate `sc`'s room the way the server does: record the same room,
/// empty, with fresh noise, wait out the start delay, then feed 60 s of the
/// node's smoothed motion to the collector.
fn calibrate(sc: &Scenario) -> Result<MotionCalibration, motion_calibration::CalibrationError> {
    let mut empty = sc.clone();
    empty.person = Person::None;
    empty.seconds =
        motion_calibration::DEFAULT_DELAY_SECS + motion_calibration::DEFAULT_CALIBRATION_SECS;
    let g = generate_with(&empty, Some(sc.seed ^ CALIBRATION_STREAM));
    let out = replay(&empty, &g, &mut Timing::default());
    let skip = (motion_calibration::DEFAULT_DELAY_SECS * sc.fs).round() as usize;
    let mut collector = motion_calibration::CalibrationCollector::default();
    for (i, f) in out.iter().enumerate().skip(skip) {
        collector.push(motion_calibration::CalibrationSample {
            smoothed_motion: f.smoothed_motion,
            rssi_dbm: NOISE_FLOOR_DBM + sc.snr_db,
            noise_floor_dbm: NOISE_FLOOR_DBM,
            sequence: i as u32,
            elapsed_secs: i as f64 / sc.fs,
        });
    }
    collector.finish()
}

/// Seconds from entry to the first presence frame, within the walk.
fn entry_latency_secs(out: &[FrameOut], fs: f64) -> Option<f64> {
    let from = (T_ENTER_SECS * fs).round() as usize;
    let to = ((T_SIT_SECS * fs).round() as usize).min(out.len());
    out[from..to]
        .iter()
        .position(|f| f.presence)
        .map(|i| round2(i as f64 / fs))
}

fn calibration_runs() -> BTreeMap<String, LinkRuns> {
    let mut out = BTreeMap::new();
    for link in &LINKS {
        let mut runs = LinkRuns {
            snr_db: link.snr_db,
            ..LinkRuns::default()
        };
        for base in scenarios()
            .into_iter()
            .filter(|s| matches!(s.family, Family::Empty | Family::Walk | Family::EnterStill))
        {
            let mut sc = base;
            sc.snr_db = link.snr_db;
            sc.agc_db = link.agc_db;
            sc.sto_ns = link.sto_ns;
            let g = generate(&sc);
            let from = (10.0 * sc.fs).round() as usize;
            let mut timing = Timing::default();
            let default = replay(&sc, &g, &mut timing);
            if sc.family == Family::EnterStill {
                runs.default_entry_secs
                    .insert(sc.name.clone(), entry_latency_secs(&default, sc.fs));
            } else {
                runs.default_pct.insert(
                    sc.name.clone(),
                    round2(share_pct(&default, from, default.len())),
                );
            }
            let threshold = match calibrate(&sc) {
                Ok(cal) => {
                    runs.presence_threshold.insert(
                        sc.name.clone(),
                        (cal.presence_threshold * 1000.0).round() / 1000.0,
                    );
                    runs.link_score
                        .insert(sc.name.clone(), cal.link_quality.score);
                    cal.presence_threshold
                }
                Err(error) => {
                    runs.refused
                        .insert(sc.name.clone(), error.code().to_owned());
                    motion_calibration::DEFAULT_PRESENCE_THRESHOLD
                }
            };
            let calibrated = replay_with_threshold(&sc, &g, &mut timing, threshold);
            if sc.family == Family::EnterStill {
                runs.calibrated_entry_secs
                    .insert(sc.name.clone(), entry_latency_secs(&calibrated, sc.fs));
            } else {
                runs.calibrated_pct.insert(
                    sc.name.clone(),
                    round2(share_pct(&calibrated, from, calibrated.len())),
                );
            }
        }
        out.insert(link.name.to_string(), runs);
    }
    out
}

// ---------------------------------------------------------------------------
// Non-human filter (plan item 1.5)
// ---------------------------------------------------------------------------

fn fan_runs() -> BTreeMap<String, FanResult> {
    let mut out = BTreeMap::new();
    for (label, db, hz) in FANS {
        let mut r = FanResult::default();
        for base in scenarios()
            .into_iter()
            .filter(|s| matches!(s.family, Family::Empty | Family::EnterStill))
        {
            let mut sc = base;
            sc.fan = Some(Fan { db, hz });
            let o = replay(&sc, &generate(&sc), &mut Timing::default());
            let at = |secs: f64| (secs * sc.fs).round() as usize;
            if sc.family == Family::Empty {
                r.empty_rooms += 1;
                if share_pct(&o, at(10.0), o.len()) > OCCUPIED_SHARE_PCT {
                    r.empty_rooms_read_occupied += 1;
                }
            } else {
                r.walk_in_rooms += 1;
                if share_pct(&o, at(100.0), o.len()) >= HELD_SHARE_PCT
                    && share_pct(&o, at(10.0), at(T_ENTER_SECS)) <= OCCUPIED_SHARE_PCT
                {
                    r.still_person_held_without_false_presence += 1;
                }
                if entry_latency_secs(&o, sc.fs).is_some_and(|s| s <= WALK_IN_SECS) {
                    r.walk_ins_caught += 1;
                }
            }
        }
        out.insert(label.to_string(), r);
    }
    out
}

// ---------------------------------------------------------------------------
// Best-link mode (plan item 1.4)
// ---------------------------------------------------------------------------

/// The three links of one room for `sc`: 1 is `sc` itself, 2 is a different
/// path the person never crosses, 3 sees the person through a noisy link.
fn three_links(sc: &Scenario) -> [Scenario; 3] {
    let seen = sc.clone();
    let mut blind = sc.clone();
    blind.seed += 50_000;
    blind.person = Person::None;
    let mut noisy = sc.clone();
    noisy.seed += 60_000;
    noisy.snr_db = 12.0;
    noisy.agc_db = 1.5;
    noisy.sto_ns = 40.0;
    [seen, blind, noisy]
}

/// Room presence per frame from three links' outputs: following one link,
/// or fusing all three the way the server does.
fn room_presence(links: &[Vec<FrameOut>; 3], follow: Option<u8>) -> Vec<bool> {
    let n = links.iter().map(Vec::len).min().unwrap_or(0);
    (0..n)
        .map(|i| match follow {
            Some(id) => links[usize::from(id - 1)][i].presence,
            None => {
                let inferences: Vec<NodeInference> = links
                    .iter()
                    .map(|l| NodeInference::new(l[i].level.as_str(), l[i].confidence, Some(0)))
                    .collect();
                fuse_room(inferences.iter(), NODE_STALE_AFTER_MS).classification != "absent"
            }
        })
        .collect()
}

fn best_link_runs(set: u64) -> BestLinkMetrics {
    let offset = set * SEED_SET_STRIDE;
    // Ranking period: five minutes in which someone enters, walks and sits.
    let mut ranking = Scenario::new(
        "ranking".into(),
        Family::EnterStill,
        offset + 900,
        Person::EnterThenStill,
    );
    ranking.seconds = 300.0;
    let mut selector = BestLink::new(false, best_link::DEFAULT_RERANK);
    let warm = (10.0 * ranking.fs) as usize;
    for (id, link) in (1u8..).zip(three_links(&ranking)) {
        let out = replay(&link, &generate(&link), &mut Timing::default());
        for f in &out[warm..] {
            selector.observe(id, f.smoothed_motion);
        }
    }
    let contrasts = selector
        .pending()
        .into_iter()
        .filter_map(|(id, (_, c))| c.map(|c| (id.to_string(), round2(c))))
        .collect();
    let chosen = selector.select(Instant::now(), &[1, 2, 3], &BTreeMap::new());

    let mut m = BestLinkMetrics {
        chosen,
        contrasts,
        ..BestLinkMetrics::default()
    };
    for sc in scenarios_for(set)
        .into_iter()
        .filter(|s| matches!(s.family, Family::Empty | Family::EnterStill))
    {
        let outs = three_links(&sc).map(|l| replay(&l, &generate(&l), &mut Timing::default()));
        let at = |secs: f64| (secs * sc.fs).round() as usize;
        for (mode, follow) in [(&mut m.best_link, chosen), (&mut m.fuse_all, None)] {
            let p = room_presence(&outs, follow);
            let share = |from: usize, to: usize| {
                let to = to.min(p.len());
                100.0 * p[from..to].iter().filter(|x| **x).count() as f64
                    / (to - from).max(1) as f64
            };
            mode.empty_rooms += usize::from(sc.family == Family::Empty);
            mode.walk_in_rooms += usize::from(sc.family == Family::EnterStill);
            match sc.family {
                Family::Empty => {
                    if share(at(10.0), p.len()) > OCCUPIED_SHARE_PCT {
                        mode.empty_rooms_read_occupied += 1;
                    }
                }
                _ => {
                    if share(at(100.0), p.len()) >= HELD_SHARE_PCT
                        && share(at(10.0), at(T_ENTER_SECS)) <= OCCUPIED_SHARE_PCT
                    {
                        mode.still_person_held_without_false_presence += 1;
                    }
                    let entered = at(T_ENTER_SECS);
                    if p[entered..at(T_ENTER_SECS + WALK_IN_SECS)]
                        .iter()
                        .any(|x| *x)
                    {
                        mode.walk_ins_caught += 1;
                    }
                }
            }
        }
    }
    m
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy)]
struct EnterStill {
    /// Presence while the room is still empty (10-30 s). Lower is better.
    empty_10_30_pct: f64,
    walking_30_40_pct: f64,
    still_40_100_pct: f64,
    still_100_240_pct: f64,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct VitalsLevel {
    /// Mean absolute breathing error over scenarios with an estimate (None
    /// when no scenario at this level produced one).
    breath_mae_bpm: Option<f64>,
    /// Scenarios with no breathing estimate in the last 15 s.
    breath_missing: usize,
    hr_mae_bpm: Option<f64>,
    hr_missing: usize,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct VitalsScenario {
    dyn_db: f64,
    breath_true_bpm: f64,
    /// Median detector estimate over the last 15 s.
    breath_est_bpm: Option<f64>,
    heart_true_bpm: f64,
    heart_est_bpm: Option<f64>,
}

#[derive(Serialize, Deserialize)]
struct Summary {
    /// Empty rooms that read as occupied (more than 5% of post-warm-up frames).
    empty_rooms_read_occupied: usize,
    empty_rooms_total: usize,
    /// Enter-then-sit scenarios where the still person is held for at least
    /// 90% of 100-240 s.
    still_person_held: usize,
    /// Of those, the ones that also kept the empty 10-30 s window under 5%.
    still_person_held_without_false_presence: usize,
    still_person_total: usize,
}

/// Calibration runs on one link. Presence shares are post-warm-up, per
/// scenario; empty rooms should read low and walking rooms high.
#[derive(Serialize, Deserialize, Default)]
struct LinkRuns {
    snr_db: f64,
    default_pct: BTreeMap<String, f64>,
    calibrated_pct: BTreeMap<String, f64>,
    /// Seconds from a person entering (enter-then-sit scenarios) to the
    /// first frame that reports presence; None when not within the 10 s walk.
    default_entry_secs: BTreeMap<String, Option<f64>>,
    calibrated_entry_secs: BTreeMap<String, Option<f64>>,
    /// Reported, not ratcheted.
    presence_threshold: BTreeMap<String, f64>,
    /// Reported, not ratcheted.
    link_score: BTreeMap<String, u8>,
    /// Refused calibrations (the run then keeps the default threshold).
    refused: BTreeMap<String, String>,
}

/// Gate G1 numbers for one seed set.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct GateSummary {
    empty_rooms_read_occupied: usize,
    empty_rooms_total: usize,
    still_person_held_without_false_presence: usize,
    still_person_total: usize,
    /// Enter-then-sit scenarios where presence came within
    /// [`WALK_IN_SECS`] of the person entering.
    walk_ins_caught: usize,
    walk_ins_total: usize,
    /// Keyed by reflection strength in dB.
    breath_mae_bpm: BTreeMap<String, Option<f64>>,
}

/// Room presence from three links (plan item 1.4).
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
struct LinkModeResult {
    empty_rooms_read_occupied: usize,
    still_person_held_without_false_presence: usize,
    walk_ins_caught: usize,
    /// Empty rooms, and walk-in-and-sit rooms, scored.
    empty_rooms: usize,
    walk_in_rooms: usize,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
struct BestLinkMetrics {
    /// Link chosen after the ranking period (1 sees the person, 2 is
    /// blind, 3 is noisy).
    chosen: Option<u8>,
    contrasts: BTreeMap<String, f64>,
    best_link: LinkModeResult,
    fuse_all: LinkModeResult,
}

/// Rooms with a running fan (plan item 1.5).
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
struct FanResult {
    empty_rooms_read_occupied: usize,
    empty_rooms: usize,
    still_person_held_without_false_presence: usize,
    walk_ins_caught: usize,
    walk_in_rooms: usize,
}

/// The fans checked, as (label, reflection dB, blade-pass Hz).
const FANS: [(&str, f64, f64); 4] = [
    ("typical box fan", -15.0, 4.2),
    ("slow fan", -15.0, 2.5),
    ("weak fan", -20.0, 2.5),
    ("strong fan", -10.0, 6.0),
];

#[derive(Serialize, Deserialize)]
struct Metrics {
    evidence: String,
    generator: String,
    summary: Summary,
    empty_false_presence_pct: BTreeMap<String, f64>,
    walk_detect_pct: BTreeMap<String, f64>,
    enter_still: BTreeMap<String, EnterStill>,
    /// Keyed by reflection strength in dB.
    vitals: BTreeMap<String, VitalsLevel>,
    /// Reported for debugging; not ratcheted.
    vitals_by_scenario: BTreeMap<String, VitalsScenario>,
    /// Heart-rate error from always guessing 80 BPM: the chance level.
    hr_chance_mae_bpm: f64,
    /// Empty-room calibration (plan item 1.1), keyed by link.
    #[serde(default)]
    calibration: BTreeMap<String, LinkRuns>,
    /// Gate G1 summary per seed set ("0", "1", "2").
    #[serde(default)]
    seed_sets: BTreeMap<String, GateSummary>,
    /// Best-link mode against fusing all links (plan item 1.4).
    #[serde(default)]
    best_link: Option<BestLinkMetrics>,
    /// Rooms with a running fan, keyed by fan label (plan item 1.5).
    #[serde(default)]
    fans: BTreeMap<String, FanResult>,
}

fn share_pct(xs: &[FrameOut], from: usize, to: usize) -> f64 {
    let to = to.min(xs.len());
    if from >= to {
        return 0.0;
    }
    100.0 * xs[from..to].iter().filter(|f| f.presence).count() as f64 / (to - from) as f64
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 0 {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    })
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn score(runs: &[(Scenario, Vec<FrameOut>)]) -> Metrics {
    let mut m = Metrics {
        evidence: "SYNTHETIC".into(),
        generator: GENERATOR_VERSION.into(),
        summary: Summary {
            empty_rooms_read_occupied: 0,
            empty_rooms_total: 0,
            still_person_held: 0,
            still_person_held_without_false_presence: 0,
            still_person_total: 0,
        },
        empty_false_presence_pct: BTreeMap::new(),
        walk_detect_pct: BTreeMap::new(),
        enter_still: BTreeMap::new(),
        vitals: BTreeMap::new(),
        vitals_by_scenario: BTreeMap::new(),
        hr_chance_mae_bpm: 0.0,
        calibration: BTreeMap::new(),
        seed_sets: BTreeMap::new(),
        best_link: None,
        fans: BTreeMap::new(),
    };
    let mut vitals_errors: BTreeMap<String, (Vec<f64>, usize, Vec<f64>, usize)> = BTreeMap::new();
    let mut chance = Vec::new();

    for (sc, out) in runs {
        let at = |s: f64| (s * sc.fs).round() as usize;
        match sc.family {
            Family::Empty => {
                let pct = round2(share_pct(out, at(10.0), out.len()));
                m.summary.empty_rooms_total += 1;
                if pct > OCCUPIED_SHARE_PCT {
                    m.summary.empty_rooms_read_occupied += 1;
                }
                m.empty_false_presence_pct.insert(sc.name.clone(), pct);
            }
            Family::Walk => {
                m.walk_detect_pct
                    .insert(sc.name.clone(), round2(share_pct(out, at(10.0), out.len())));
            }
            Family::EnterStill => {
                let e = EnterStill {
                    empty_10_30_pct: round2(share_pct(out, at(10.0), at(30.0))),
                    walking_30_40_pct: round2(share_pct(out, at(30.0), at(40.0))),
                    still_40_100_pct: round2(share_pct(out, at(40.0), at(100.0))),
                    still_100_240_pct: round2(share_pct(out, at(100.0), out.len())),
                };
                m.summary.still_person_total += 1;
                if e.still_100_240_pct >= HELD_SHARE_PCT {
                    m.summary.still_person_held += 1;
                    if e.empty_10_30_pct <= OCCUPIED_SHARE_PCT {
                        m.summary.still_person_held_without_false_presence += 1;
                    }
                }
                m.enter_still.insert(sc.name.clone(), e);
            }
            Family::Vitals => {
                let tail = &out[out.len().saturating_sub(at(15.0))..];
                let br = median(tail.iter().filter_map(|f| f.detector_br).collect());
                let hr = median(tail.iter().filter_map(|f| f.detector_hr).collect());
                let entry = vitals_errors
                    .entry(format!("{:.0}dB", sc.dyn_db))
                    .or_insert_with(|| (Vec::new(), 0, Vec::new(), 0));
                match br {
                    Some(b) => entry.0.push((b - sc.breath_bpm).abs()),
                    None => entry.1 += 1,
                }
                match hr {
                    Some(h) => entry.2.push((h - sc.heart_bpm).abs()),
                    None => entry.3 += 1,
                }
                chance.push((sc.heart_bpm - 80.0).abs());
                m.vitals_by_scenario.insert(
                    sc.name.clone(),
                    VitalsScenario {
                        dyn_db: sc.dyn_db,
                        breath_true_bpm: sc.breath_bpm,
                        breath_est_bpm: br.map(round2),
                        heart_true_bpm: sc.heart_bpm,
                        heart_est_bpm: hr.map(round2),
                    },
                );
            }
        }
    }
    let mean = |v: &[f64]| (!v.is_empty()).then(|| round2(v.iter().sum::<f64>() / v.len() as f64));
    for (level, (br, br_missing, hr, hr_missing)) in vitals_errors {
        m.vitals.insert(
            level,
            VitalsLevel {
                breath_mae_bpm: mean(&br),
                breath_missing: br_missing,
                hr_mae_bpm: mean(&hr),
                hr_missing,
            },
        );
    }
    m.hr_chance_mae_bpm = mean(&chance).unwrap_or(0.0);
    m
}

/// G1 numbers for one seed set: `m` is [`score`] of the same `runs`, which
/// are needed only for the walk-in latencies.
fn gate_summary(m: &Metrics, runs: &[(Scenario, Vec<FrameOut>)]) -> GateSummary {
    let entries: Vec<Option<f64>> = runs
        .iter()
        .filter(|(sc, _)| sc.family == Family::EnterStill)
        .map(|(sc, out)| entry_latency_secs(out, sc.fs))
        .collect();
    GateSummary {
        empty_rooms_read_occupied: m.summary.empty_rooms_read_occupied,
        empty_rooms_total: m.summary.empty_rooms_total,
        still_person_held_without_false_presence: m
            .summary
            .still_person_held_without_false_presence,
        still_person_total: m.summary.still_person_total,
        walk_ins_caught: entries
            .iter()
            .filter(|e| e.is_some_and(|s| s <= WALK_IN_SECS))
            .count(),
        walk_ins_total: entries.len(),
        breath_mae_bpm: m
            .vitals
            .iter()
            .map(|(level, v)| (level.clone(), v.breath_mae_bpm))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Ratchet against the committed baseline
// ---------------------------------------------------------------------------

fn regressions(now: &Metrics, base: &Metrics) -> Vec<String> {
    let mut bad = Vec::new();
    for (name, b) in &base.empty_false_presence_pct {
        let n = now
            .empty_false_presence_pct
            .get(name)
            .copied()
            .unwrap_or(f64::INFINITY);
        if n > b + TOL_PCT {
            bad.push(format!(
                "{name}: empty-room false presence {n:.1}% (baseline {b:.1}%)"
            ));
        }
    }
    for (name, b) in &base.walk_detect_pct {
        let n = now.walk_detect_pct.get(name).copied().unwrap_or(0.0);
        if n < b - TOL_PCT {
            bad.push(format!(
                "{name}: walking detection {n:.1}% (baseline {b:.1}%)"
            ));
        }
    }
    for (name, b) in &base.enter_still {
        let Some(n) = now.enter_still.get(name) else {
            bad.push(format!("{name}: missing"));
            continue;
        };
        if n.empty_10_30_pct > b.empty_10_30_pct + TOL_PCT {
            bad.push(format!(
                "{name}: false presence before entry {:.1}% (baseline {:.1}%)",
                n.empty_10_30_pct, b.empty_10_30_pct
            ));
        }
        for (label, nv, bv) in [
            ("walking 30-40 s", n.walking_30_40_pct, b.walking_30_40_pct),
            ("still 40-100 s", n.still_40_100_pct, b.still_40_100_pct),
            ("still 100-240 s", n.still_100_240_pct, b.still_100_240_pct),
        ] {
            if nv < bv - TOL_PCT {
                bad.push(format!(
                    "{name}: presence {label} {nv:.1}% (baseline {bv:.1}%)"
                ));
            }
        }
    }
    for (link, b) in &base.calibration {
        let Some(n) = now.calibration.get(link) else {
            bad.push(format!("calibration {link}: missing"));
            continue;
        };
        for (label, nmap, bmap) in [
            ("default", &n.default_pct, &b.default_pct),
            ("calibrated", &n.calibrated_pct, &b.calibrated_pct),
        ] {
            for (name, bv) in bmap {
                let empty = name.starts_with("empty");
                let nv = nmap
                    .get(name)
                    .copied()
                    .unwrap_or(if empty { f64::INFINITY } else { 0.0 });
                let worse = if empty {
                    nv > bv + TOL_PCT
                } else {
                    nv < bv - TOL_PCT
                };
                if worse {
                    bad.push(format!(
                        "calibration {link} {name} ({label} threshold): presence {nv:.1}% (baseline {bv:.1}%)"
                    ));
                }
            }
        }
        for (label, nmap, bmap) in [
            ("default", &n.default_entry_secs, &b.default_entry_secs),
            (
                "calibrated",
                &n.calibrated_entry_secs,
                &b.calibrated_entry_secs,
            ),
        ] {
            for (name, bv) in bmap {
                let nv = nmap.get(name).copied().flatten();
                let later = match (nv, bv) {
                    (None, Some(_)) => true,
                    (Some(n), Some(b)) => n > b + 0.5,
                    _ => false,
                };
                if later {
                    bad.push(format!(
                        "calibration {link} {name} ({label} threshold): walk-in detected after {nv:?} s (baseline {bv:?} s)"
                    ));
                }
            }
        }
    }
    for (set, b) in &base.seed_sets {
        let Some(n) = now.seed_sets.get(set) else {
            bad.push(format!("seed set {set}: missing"));
            continue;
        };
        if n.empty_rooms_read_occupied > b.empty_rooms_read_occupied {
            bad.push(format!(
                "seed set {set}: {} empty rooms read occupied (baseline {})",
                n.empty_rooms_read_occupied, b.empty_rooms_read_occupied
            ));
        }
        if n.still_person_held_without_false_presence < b.still_person_held_without_false_presence {
            bad.push(format!(
                "seed set {set}: still person held in {} (baseline {})",
                n.still_person_held_without_false_presence,
                b.still_person_held_without_false_presence
            ));
        }
        if n.walk_ins_caught < b.walk_ins_caught {
            bad.push(format!(
                "seed set {set}: {} walk-ins caught (baseline {})",
                n.walk_ins_caught, b.walk_ins_caught
            ));
        }
        for (level, bv) in &b.breath_mae_bpm {
            let nv = n.breath_mae_bpm.get(level).copied().flatten();
            match (nv, bv) {
                (Some(nv), Some(bv)) if nv > bv + TOL_BPM => bad.push(format!(
                    "seed set {set}: breathing error at {level} {nv:.2} BPM (baseline {bv:.2})"
                )),
                (None, Some(bv)) => bad.push(format!(
                    "seed set {set}: no breathing estimate at {level} (baseline {bv:.2} BPM)"
                )),
                _ => {}
            }
        }
    }
    for (label, b) in &base.fans {
        let Some(n) = now.fans.get(label) else {
            bad.push(format!("fan {label}: missing"));
            continue;
        };
        if n.empty_rooms_read_occupied > b.empty_rooms_read_occupied
            || n.still_person_held_without_false_presence
                < b.still_person_held_without_false_presence
            || n.walk_ins_caught < b.walk_ins_caught
        {
            bad.push(format!(
                "fan {label}: empty occupied {} / still held {} / walk-ins {} (baseline {} / {} / {})",
                n.empty_rooms_read_occupied,
                n.still_person_held_without_false_presence,
                n.walk_ins_caught,
                b.empty_rooms_read_occupied,
                b.still_person_held_without_false_presence,
                b.walk_ins_caught
            ));
        }
    }
    if let (Some(b), Some(n)) = (&base.best_link, &now.best_link) {
        if n.chosen != b.chosen {
            bad.push(format!(
                "best link: chose {:?} (baseline {:?})",
                n.chosen, b.chosen
            ));
        }
        for (label, nm, bm) in [
            ("best link", &n.best_link, &b.best_link),
            ("fuse all", &n.fuse_all, &b.fuse_all),
        ] {
            if nm.empty_rooms_read_occupied > bm.empty_rooms_read_occupied
                || nm.still_person_held_without_false_presence
                    < bm.still_person_held_without_false_presence
                || nm.walk_ins_caught < bm.walk_ins_caught
            {
                bad.push(format!(
                    "{label}: empty occupied {} / still held {} / walk-ins {} (baseline {} / {} / {})",
                    nm.empty_rooms_read_occupied,
                    nm.still_person_held_without_false_presence,
                    nm.walk_ins_caught,
                    bm.empty_rooms_read_occupied,
                    bm.still_person_held_without_false_presence,
                    bm.walk_ins_caught
                ));
            }
        }
    } else if base.best_link.is_some() {
        bad.push("best link: missing".into());
    }
    // Heart rate is not ratcheted: it is at chance level and is being hidden
    // (plan item 0.4), so its number is reported only.
    for (level, b) in &base.vitals {
        let Some(n) = now.vitals.get(level) else {
            bad.push(format!("vitals {level}: missing"));
            continue;
        };
        if n.breath_missing > b.breath_missing {
            bad.push(format!(
                "vitals {level}: {} scenarios without a breathing estimate (baseline {})",
                n.breath_missing, b.breath_missing
            ));
        }
        if let Some(bb) = b.breath_mae_bpm {
            match n.breath_mae_bpm {
                Some(nb) if nb <= bb + TOL_BPM => {}
                Some(nb) => bad.push(format!(
                    "vitals {level}: breathing error {nb:.2} BPM (baseline {bb:.2})"
                )),
                None => bad.push(format!(
                    "vitals {level}: no breathing estimates (baseline error {bb:.2} BPM)"
                )),
            }
        }
    }
    bad
}

/// Plan item 4.1 (review R11): the vitals crate holds a second, older
/// pipeline (`CsiVitalPreprocessor` + `BreathingExtractor` +
/// `HeartRateExtractor`, used by `ruview-kernel` and the Python bindings).
/// Run it on the same vitals scenarios as the detector, in all seed sets,
/// with `ruview-kernel`'s default settings, and score both the same way:
/// the median of the last 15 s, against the true rate. Reported only; it is
/// not part of [`Metrics`], so it is never ratcheted.
fn older_pipeline_comparison() -> String {
    use wifi_densepose_vitals::{
        BreathingExtractor, CsiFrame, CsiVitalPreprocessor, HeartRateExtractor, VitalStatus,
    };
    #[derive(Default)]
    struct Errors {
        detector_br: Vec<f64>,
        detector_br_missing: usize,
        older_br: Vec<f64>,
        older_br_missing: usize,
        detector_hr: Vec<f64>,
        older_hr: Vec<f64>,
        older_hr_missing: usize,
        detector_hr_missing: usize,
    }
    fn push(est: Option<f64>, truth: f64, errors: &mut Vec<f64>, missing: &mut usize) {
        match est {
            Some(x) => errors.push((x - truth).abs()),
            None => *missing += 1,
        }
    }
    let mut levels: BTreeMap<String, Errors> = BTreeMap::new();
    let mut cases = 0;
    for set in 0..SEED_SETS {
        for sc in scenarios_for(set)
            .into_iter()
            .filter(|s| s.family == Family::Vitals)
        {
            let g = generate(&sc);
            let n = g.amplitudes[0].len();
            let mut detector = VitalSignDetector::new(sc.fs);
            // ruview-kernel's SessionConfig defaults.
            let mut pre = CsiVitalPreprocessor::new(n, 0.05);
            let mut breathing = BreathingExtractor::new(n, sc.fs, 30.0);
            let mut heart = HeartRateExtractor::new(n, sc.fs, 15.0);
            let weights = vec![1.0 / n as f64; n];
            let tail_from = g.amplitudes.len().saturating_sub((15.0 * sc.fs) as usize);
            let (mut d_br, mut d_hr, mut o_br, mut o_hr) = (vec![], vec![], vec![], vec![]);
            let (mut last_br, mut last_hr) = (None, None);
            for (i, (a, p)) in g.amplitudes.iter().zip(&g.phases).enumerate() {
                let v = detector.process_frame(a, p);
                let frame = CsiFrame {
                    amplitudes: a.clone(),
                    phases: p.clone(),
                    n_subcarriers: n,
                    sample_index: i as u64,
                    sample_rate_hz: sc.fs,
                };
                if let Some(residuals) = pre.process(&frame) {
                    if let Some(e) = breathing.extract(&residuals, &weights) {
                        last_br = (e.status != VitalStatus::Unavailable).then_some(e.value_bpm);
                    }
                    if let Some(e) = heart.extract(&residuals, &frame.phases) {
                        last_hr = (e.status != VitalStatus::Unavailable).then_some(e.value_bpm);
                    }
                }
                if i >= tail_from {
                    d_br.extend(v.breathing_rate_bpm);
                    d_hr.extend(v.heart_rate_bpm);
                    o_br.extend(last_br);
                    o_hr.extend(last_hr);
                }
            }
            cases += 1;
            let e = levels.entry(format!("{:.0} dB", sc.dyn_db)).or_default();
            push(
                median(d_br),
                sc.breath_bpm,
                &mut e.detector_br,
                &mut e.detector_br_missing,
            );
            push(
                median(o_br),
                sc.breath_bpm,
                &mut e.older_br,
                &mut e.older_br_missing,
            );
            push(
                median(d_hr),
                sc.heart_bpm,
                &mut e.detector_hr,
                &mut e.detector_hr_missing,
            );
            push(
                median(o_hr),
                sc.heart_bpm,
                &mut e.older_hr,
                &mut e.older_hr_missing,
            );
        }
    }
    let mae = |v: &[f64]| {
        if v.is_empty() {
            "—".to_string()
        } else {
            format!("{:.2}", v.iter().sum::<f64>() / v.len() as f64)
        }
    };
    let mut r = String::new();
    let _ = writeln!(
        r,
        "\n### Older vitals pipeline in `wifi-densepose-vitals` (informational, not ratcheted)\n"
    );
    let _ = writeln!(
        r,
        "The vital-sign detector against the crate's `BreathingExtractor` / `HeartRateExtractor` \
         (as `ruview-kernel` runs them), on the {cases} vitals scenarios of seed sets 0–{}. \
         Mean absolute error in BPM; misses (no estimate in the last 15 s) in brackets.\n",
        SEED_SETS - 1
    );
    let _ = writeln!(
        r,
        "| Reflection | Breathing, detector | Breathing, older pipeline | Heart rate, detector | Heart rate, older pipeline |\n|---|---|---|---|---|"
    );
    for (level, e) in &levels {
        let _ = writeln!(
            r,
            "| {level} | {} ({}) | {} ({}) | {} ({}) | {} ({}) |",
            mae(&e.detector_br),
            e.detector_br_missing,
            mae(&e.older_br),
            e.older_br_missing,
            mae(&e.detector_hr),
            e.detector_hr_missing,
            mae(&e.older_hr),
            e.older_hr_missing
        );
    }
    r
}

fn report_markdown(m: &Metrics, t: &Timing) -> String {
    let s = &m.summary;
    let mut r = String::new();
    let _ = writeln!(r, "## Scenario harness (SYNTHETIC, `{}`)\n", m.generator);
    let _ = writeln!(
        r,
        "Accuracy numbers are SYNTHETIC: simulated rooms, not real ones. Timings are MEASURED on this runner.\n"
    );
    let _ = writeln!(r, "| Check | Result |\n|---|---|");
    let _ = writeln!(
        r,
        "| Empty rooms that read occupied (>{OCCUPIED_SHARE_PCT}% of frames) | {} of {} |",
        s.empty_rooms_read_occupied, s.empty_rooms_total
    );
    let _ = writeln!(
        r,
        "| Still person held (>={HELD_SHARE_PCT}% of 100-240 s) | {} of {} |",
        s.still_person_held, s.still_person_total
    );
    let _ = writeln!(
        r,
        "| ...and room not occupied before entry | {} of {} |",
        s.still_person_held_without_false_presence, s.still_person_total
    );
    let bpm = |x: Option<f64>| x.map_or("n/a".to_string(), |v| format!("{v:.2} BPM"));
    for (level, v) in &m.vitals {
        let _ = writeln!(
            r,
            "| Breathing error at {level} reflection | {} ({} of 8 without an estimate) |",
            bpm(v.breath_mae_bpm),
            v.breath_missing
        );
    }
    for (level, v) in &m.vitals {
        let _ = writeln!(
            r,
            "| Heart-rate error at {level} (chance: {:.2} BPM) | {} |",
            m.hr_chance_mae_bpm,
            bpm(v.hr_mae_bpm)
        );
    }
    let per = |x: f64| x / t.frames.max(1) as f64;
    let _ = writeln!(
        r,
        "| Per-frame time, features / vitals / total (MEASURED) | {:.0} / {:.0} / {:.0} µs |",
        per(t.features_us),
        per(t.vitals_us),
        per(t.total_us)
    );
    if !m.seed_sets.is_empty() {
        let _ = writeln!(
            r,
            "\n### Gate G1 across {} seed sets (SYNTHETIC)\n",
            m.seed_sets.len()
        );
        let sets: Vec<&String> = m.seed_sets.keys().collect();
        let _ = writeln!(
            r,
            "| Criterion | Target | {} | Met |\n|---|---|{}---|",
            sets.iter()
                .map(|s| format!("Set {s}"))
                .collect::<Vec<_>>()
                .join(" | "),
            "---|".repeat(sets.len())
        );
        let mut row = |label: &str, target: &str, cells: Vec<(String, bool)>| {
            let met = cells.iter().all(|(_, ok)| *ok);
            let _ = writeln!(
                r,
                "| {label} | {target} | {} | {} |",
                cells
                    .iter()
                    .map(|(c, _)| c.as_str())
                    .collect::<Vec<_>>()
                    .join(" | "),
                if met { "yes" } else { "no" }
            );
        };
        row(
            "Empty rooms read occupied",
            "0",
            m.seed_sets
                .values()
                .map(|g| {
                    (
                        format!("{} of {}", g.empty_rooms_read_occupied, g.empty_rooms_total),
                        g.empty_rooms_read_occupied == 0,
                    )
                })
                .collect(),
        );
        row(
            "Still person held, no false presence before entry",
            "all",
            m.seed_sets
                .values()
                .map(|g| {
                    (
                        format!(
                            "{} of {}",
                            g.still_person_held_without_false_presence, g.still_person_total
                        ),
                        g.still_person_held_without_false_presence == g.still_person_total,
                    )
                })
                .collect(),
        );
        row(
            &format!("Walk-ins caught within {WALK_IN_SECS:.0} s"),
            "all",
            m.seed_sets
                .values()
                .map(|g| {
                    (
                        format!("{} of {}", g.walk_ins_caught, g.walk_ins_total),
                        g.walk_ins_caught == g.walk_ins_total,
                    )
                })
                .collect(),
        );
        let levels: std::collections::BTreeSet<String> = m
            .seed_sets
            .values()
            .flat_map(|g| g.breath_mae_bpm.keys().cloned())
            .collect();
        for level in levels {
            row(
                &format!("Breathing error at {level}"),
                &format!("≤ {G1_BREATH_MAE_BPM} BPM"),
                m.seed_sets
                    .values()
                    .map(|g| match g.breath_mae_bpm.get(&level).copied().flatten() {
                        Some(v) => (format!("{v:.2}"), v <= G1_BREATH_MAE_BPM),
                        None => ("none".into(), false),
                    })
                    .collect(),
            );
        }
        let per_frame = t.total_us / t.frames.max(1) as f64;
        let _ = writeln!(
            r,
            "| Per-frame time, all sets (MEASURED) | ≤ {G1_FRAME_US:.0} µs | {per_frame:.0} µs{} | {} |",
            " | ".repeat(sets.len().saturating_sub(1)),
            if per_frame <= G1_FRAME_US { "yes" } else { "no" }
        );
    }
    if !m.fans.is_empty() {
        let _ = writeln!(
            r,
            "\n### Rooms with a running fan (plan item 1.5, SYNTHETIC)\n"
        );
        let _ = writeln!(
            r,
            "| Fan | Reflection / blade rate | Empty rooms occupied | Still person held, no false presence before entry | Walk-ins caught within {WALK_IN_SECS:.0} s |\n|---|---|---|---|---|"
        );
        for (label, db, hz) in FANS {
            let Some(f) = m.fans.get(label) else { continue };
            let _ = writeln!(
                r,
                "| {label} | {db:.0} dB / {hz} Hz | {} of {} | {} of {} | {} of {} |",
                f.empty_rooms_read_occupied,
                f.empty_rooms,
                f.still_person_held_without_false_presence,
                f.walk_in_rooms,
                f.walk_ins_caught,
                f.walk_in_rooms
            );
        }
        let _ = writeln!(
            r,
            "\nThe strong fan is stronger than the person's reflection; removing it would hide people, so it is reported and its rooms read occupied."
        );
    }
    if let Some(b) = &m.best_link {
        let _ = writeln!(r, "\n### Best-link mode (plan item 1.4, SYNTHETIC)\n");
        let _ = writeln!(
            r,
            "Three links on one room: 1 sees the person (25 dB SNR), 2 never does, 3 sees them through a noisy link (12 dB). \
             A five-minute ranking period chose link {} (contrast {}).\n",
            b.chosen.map_or("none".into(), |c| c.to_string()),
            b.contrasts
                .iter()
                .map(|(k, v)| format!("{k}: {v:.1}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(
            r,
            "| Room verdict | Empty rooms occupied | Still person held, no false presence before entry | Walk-ins caught within {WALK_IN_SECS:.0} s |\n|---|---|---|---|"
        );
        for (label, x) in [
            ("Best link (default)", &b.best_link),
            ("Fuse all links", &b.fuse_all),
        ] {
            let _ = writeln!(
                r,
                "| {label} | {} of {} | {} of {} | {} of {} |",
                x.empty_rooms_read_occupied,
                x.empty_rooms,
                x.still_person_held_without_false_presence,
                x.walk_in_rooms,
                x.walk_ins_caught,
                x.walk_in_rooms
            );
        }
    }
    if !m.calibration.is_empty() {
        let _ = writeln!(
            r,
            "\n### Empty-room calibration (plan item 1.1, SYNTHETIC)\n"
        );
        let _ = writeln!(
            r,
            "Each room is calibrated from 60 s of the same room, empty, with fresh noise. \
             Presence shares are of post-warm-up frames.\n"
        );
        let _ = writeln!(
            r,
            "| Link | Empty rooms occupied, default → calibrated | Walking frames detected, default → calibrated | Walk-ins caught within {WALK_IN_SECS:.0} s, default → calibrated | Calibrated threshold | Link score | Refused |\n|---|---|---|---|---|---|---|"
        );
        let span = |xs: Vec<f64>, digits: usize| -> String {
            let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if xs.is_empty() {
                "n/a".into()
            } else if (hi - lo).abs() < 1e-9 {
                format!("{lo:.digits$}")
            } else {
                format!("{lo:.digits$}–{hi:.digits$}")
            }
        };
        for link in LINKS.iter().map(|l| l.name) {
            let Some(c) = m.calibration.get(link) else {
                continue;
            };
            let occupied = |map: &BTreeMap<String, f64>| {
                let empty: Vec<f64> = map
                    .iter()
                    .filter(|(k, _)| k.starts_with("empty"))
                    .map(|(_, v)| *v)
                    .collect();
                format!(
                    "{} of {}",
                    empty.iter().filter(|v| **v > OCCUPIED_SHARE_PCT).count(),
                    empty.len()
                )
            };
            let walking = |map: &BTreeMap<String, f64>| {
                span(
                    map.iter()
                        .filter(|(k, _)| k.starts_with("walk"))
                        .map(|(_, v)| *v)
                        .collect(),
                    1,
                ) + "%"
            };
            let caught = |map: &BTreeMap<String, Option<f64>>| {
                format!(
                    "{} of {}",
                    map.values()
                        .filter(|v| v.is_some_and(|s| s <= WALK_IN_SECS))
                        .count(),
                    map.len()
                )
            };
            let _ = writeln!(
                r,
                "| {link} ({:.0} dB SNR) | {} → {} | {} → {} | {} → {} | {} | {} | {} of {} |",
                c.snr_db,
                occupied(&c.default_pct),
                occupied(&c.calibrated_pct),
                walking(&c.default_pct),
                walking(&c.calibrated_pct),
                caught(&c.default_entry_secs),
                caught(&c.calibrated_entry_secs),
                span(c.presence_threshold.values().copied().collect(), 3),
                span(c.link_score.values().map(|v| f64::from(*v)).collect(), 0),
                c.refused.len(),
                c.presence_threshold.len() + c.refused.len()
            );
        }
        let _ = writeln!(
            r,
            "\nThe default threshold is {}. On the noisy link it reads every frame as present, empty or not, \
             so its walking and walk-in numbers there mean nothing.",
            motion_calibration::DEFAULT_PRESENCE_THRESHOLD
        );
    }
    let _ = writeln!(r, "\n<details><summary>Per-scenario presence</summary>\n");
    let _ = writeln!(r, "| Scenario | Presence |\n|---|---|");
    for (name, v) in &m.empty_false_presence_pct {
        let _ = writeln!(r, "| {name} (empty) | {v:.1}% |");
    }
    for (name, v) in &m.walk_detect_pct {
        let _ = writeln!(r, "| {name} (walking) | {v:.1}% |");
    }
    for (name, e) in &m.enter_still {
        let _ = writeln!(
            r,
            "| {name} (empty 10-30 / walk / still 40-100 / still 100-240) | {:.1}% / {:.1}% / {:.1}% / {:.1}% |",
            e.empty_10_30_pct, e.walking_30_40_pct, e.still_40_100_pct, e.still_100_240_pct
        );
    }
    let _ = writeln!(
        r,
        "\n</details>\n\n<details><summary>Per-scenario vital signs</summary>\n"
    );
    let _ = writeln!(r, "| Scenario | Reflection | Breathing true / estimated | Heart rate true / estimated |\n|---|---|---|---|");
    let est = |x: Option<f64>| x.map_or("none".to_string(), |v| format!("{v:.1}"));
    for (name, v) in &m.vitals_by_scenario {
        let _ = writeln!(
            r,
            "| {name} | {:.0} dB | {:.1} / {} | {:.1} / {} |",
            v.dyn_db,
            v.breath_true_bpm,
            est(v.breath_est_bpm),
            v.heart_true_bpm,
            est(v.heart_est_bpm)
        );
    }
    let _ = writeln!(r, "\n</details>");
    r
}

#[test]
fn generator_is_deterministic_and_scenario_set_is_stable() {
    let all = scenarios();
    assert_eq!(all.len(), 39);
    let sc = &all[0];
    let mut short = sc.clone();
    short.seconds = 2.0;
    let a = generate(&short);
    let b = generate(&short);
    assert_eq!(a.amplitudes, b.amplitudes);
    assert_eq!(a.phases, b.phases);
    assert_eq!(a.amplitudes.len(), 40);
    assert_eq!(a.amplitudes[0].len(), 52);
    // int8 I/Q: amplitudes are bounded by the quantizer.
    assert!(a
        .amplitudes
        .iter()
        .flatten()
        .all(|x| *x <= 128.0 * std::f64::consts::SQRT_2));
}

#[test]
fn seed_sets_change_only_the_seeds() {
    let base = scenarios();
    let other = scenarios_for(1);
    assert_eq!(base.len(), other.len());
    for (a, b) in base.iter().zip(&other) {
        assert_eq!(a.name, b.name);
        assert!(a.family == b.family && a.person == b.person);
        assert_eq!(a.seconds, b.seconds);
        assert_eq!(a.dyn_db, b.dyn_db);
        assert_eq!(b.seed, a.seed + SEED_SET_STRIDE);
    }
    // New breathing and heart rates come from the offset picker.
    assert!(base
        .iter()
        .zip(&other)
        .any(|(a, b)| a.breath_bpm != b.breath_bpm));
}

#[test]
fn calibration_segment_records_the_same_room_with_fresh_noise() {
    let mut sc = scenarios().remove(0);
    sc.seconds = 2.0;
    let a = generate(&sc);
    let b = generate_with(&sc, Some(sc.seed ^ CALIBRATION_STREAM));
    assert_ne!(a.amplitudes, b.amplitudes, "noise must differ");
    let profile = |g: &Generated| -> Vec<f64> {
        (0..g.amplitudes[0].len())
            .map(|j| g.amplitudes.iter().map(|f| f[j]).sum::<f64>() / g.amplitudes.len() as f64)
            .collect()
    };
    let (pa, pb) = (profile(&a), profile(&b));
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (ma, mb) = (mean(&pa), mean(&pb));
    let cov: f64 = pa.iter().zip(&pb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let var = |v: &[f64], m: f64| v.iter().map(|x| (x - m).powi(2)).sum::<f64>();
    let corr = cov / (var(&pa, ma) * var(&pb, mb)).sqrt();
    assert!(
        corr > 0.95,
        "same room should give the same amplitude profile, corr {corr}"
    );
}

#[test]
#[ignore = "slow in debug builds; CI runs it with --release (scenario-harness.yml)"]
fn scenario_harness() {
    let mut timing = Timing::default();
    let run_set = |set: u64, timing: &mut Timing| -> Vec<(Scenario, Vec<FrameOut>)> {
        scenarios_for(set)
            .into_iter()
            .map(|sc| {
                let g = generate(&sc);
                let out = replay(&sc, &g, timing);
                (sc, out)
            })
            .collect()
    };
    let runs = run_set(0, &mut timing);
    let mut metrics = score(&runs);
    let set0 = gate_summary(&metrics, &runs);
    metrics.seed_sets.insert("0".into(), set0);
    for set in 1..SEED_SETS {
        let other = run_set(set, &mut timing);
        let summary = gate_summary(&score(&other), &other);
        metrics.seed_sets.insert(set.to_string(), summary);
    }
    metrics.calibration = calibration_runs();
    metrics.best_link = Some(best_link_runs(0));
    metrics.fans = fan_runs();
    let report = report_markdown(&metrics, &timing) + &older_pipeline_comparison();
    eprintln!("{report}");
    if let Ok(path) = std::env::var("RUVIEW_SCENARIO_REPORT") {
        std::fs::write(&path, &report).expect("write RUVIEW_SCENARIO_REPORT");
    }

    let baseline_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scenario_baseline.json");
    if std::env::var("RUVIEW_SCENARIO_UPDATE_BASELINE").as_deref() == Ok("1") {
        let json = serde_json::to_string_pretty(&metrics).expect("serialize metrics");
        std::fs::write(&baseline_path, json + "\n").expect("write baseline");
        eprintln!("baseline written to {}", baseline_path.display());
        return;
    }
    let base: Metrics = serde_json::from_str(
        &std::fs::read_to_string(&baseline_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", baseline_path.display())),
    )
    .expect("parse baseline");
    assert_eq!(
        base.generator, GENERATOR_VERSION,
        "baseline was produced by a different generator; refresh it with RUVIEW_SCENARIO_UPDATE_BASELINE=1"
    );
    let bad = regressions(&metrics, &base);
    assert!(
        bad.is_empty(),
        "SYNTHETIC scenario metrics got worse than the committed baseline:\n  {}\n\
         If the change is intentional, refresh the baseline with RUVIEW_SCENARIO_UPDATE_BASELINE=1 and explain why in the PR.",
        bad.join("\n  ")
    );
}

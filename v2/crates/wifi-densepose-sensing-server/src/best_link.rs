//! Best-link mode (plan item 1.4).
//!
//! In a 12-day ESP32 study, one well-placed link beat 72 links fused
//! together, and the best link changed about every three days. A link only
//! senses people whose movement crosses its path, so a node that cannot see
//! the person dilutes the room vote. By default the room presence therefore
//! follows the single link that reacts most strongly to activity, and the
//! choice is re-ranked on a schedule (weekly by default). Fusing every link
//! stays available behind `--fuse-all-links`.
//!
//! The first rankings come sooner (1 h, 4 h, 16 h and 64 h after the
//! previous one) so a choice made from the first minutes is not kept for a
//! week, and a ranking in which no link saw clear activity keeps fusing.
//!
//! Links are ranked by **contrast**: the 99th percentile of a node's
//! smoothed motion score over the ranking period divided by its 10th
//! percentile. A link that sees people swings far above its own quiet floor;
//! a blind link stays near it, and a noisy link's floor is high. Ties go to
//! the higher link-quality score (from empty-room calibration), then the
//! lower node id. The rule is a starting point tuned on SYNTHETIC rooms, not
//! yet MEASURED on hardware.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Default time between re-rankings.
pub const DEFAULT_RERANK: Duration = Duration::from_secs(7 * 24 * 3600);
/// Frames a node needs in the ranking period before it can be chosen
/// (one minute at 20 Hz).
pub const MIN_RANK_FRAMES: u64 = 1200;
/// Contrasts within this factor of the best are treated as a tie.
const CONTRAST_TIE: f64 = 1.1;
/// Below this best contrast no link saw clear activity, so none is chosen.
/// SYNTHETIC: a link that sees a walk-in reaches about 11, a blind one 2.
pub const MIN_CONTRAST: f64 = 3.0;
/// Interval before the first re-rank; each later one is 4x longer, up to the
/// configured interval.
const FIRST_RERANK: Duration = Duration::from_secs(3600);

/// Log-spaced histogram of smoothed motion, so percentiles over a week cost
/// a few hundred bytes per node.
#[derive(Debug, Clone)]
pub struct MotionHistogram {
    counts: [u64; BINS],
    total: u64,
}

const BINS: usize = 96;
/// Motion scores are clamped into [MIN_MOTION, 1] before binning.
const MIN_MOTION: f64 = 1e-4;

impl Default for MotionHistogram {
    fn default() -> Self {
        Self {
            counts: [0; BINS],
            total: 0,
        }
    }
}

impl MotionHistogram {
    fn bin(motion: f64) -> usize {
        let x = motion.clamp(MIN_MOTION, 1.0);
        let pos = (x / MIN_MOTION).ln() / (1.0 / MIN_MOTION).ln();
        ((pos * (BINS - 1) as f64).round() as usize).min(BINS - 1)
    }

    fn value(bin: usize) -> f64 {
        MIN_MOTION * (1.0 / MIN_MOTION).powf(bin as f64 / (BINS - 1) as f64)
    }

    pub fn push(&mut self, motion: f64) {
        if motion.is_finite() {
            self.counts[Self::bin(motion)] += 1;
            self.total += 1;
        }
    }

    pub fn frames(&self) -> u64 {
        self.total
    }

    /// Approximate percentile (0-1); `None` when empty.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let target = ((self.total - 1) as f64 * p.clamp(0.0, 1.0)).round() as u64;
        let mut seen = 0;
        for (bin, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen > target {
                return Some(Self::value(bin));
            }
        }
        Some(Self::value(BINS - 1))
    }

    /// p99 / p10 of the smoothed motion.
    pub fn contrast(&self) -> Option<f64> {
        let lo = self.percentile(0.10)?;
        let hi = self.percentile(0.99)?;
        Some(hi / lo.max(MIN_MOTION))
    }
}

/// Picks the link that room presence follows.
#[derive(Debug, Clone)]
pub struct BestLink {
    fuse_all: bool,
    rerank_every: Duration,
    selected: Option<u8>,
    selected_at: Option<Instant>,
    /// Ranking evidence gathered since the last ranking.
    stats: BTreeMap<u8, MotionHistogram>,
    /// What the last ranking saw, for the status endpoint.
    last_ranking: BTreeMap<u8, RankEntry>,
    /// Rankings done so far; sets the early, shorter intervals.
    rankings: u32,
}

/// One node's standing at the last ranking.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RankEntry {
    pub contrast: Option<f64>,
    pub frames: u64,
    pub link_score: Option<u8>,
}

impl BestLink {
    pub fn new(fuse_all: bool, rerank_every: Duration) -> Self {
        Self {
            fuse_all,
            rerank_every,
            selected: None,
            selected_at: None,
            stats: BTreeMap::new(),
            last_ranking: BTreeMap::new(),
            rankings: 0,
        }
    }

    /// Interval after the current ranking: 1 h, 4 h, 16 h, 64 h, then the
    /// configured interval (never longer than it).
    fn interval(&self) -> Duration {
        let early =
            FIRST_RERANK.saturating_mul(4u32.saturating_pow(self.rankings.saturating_sub(1)));
        early.min(self.rerank_every)
    }

    pub fn fuse_all(&self) -> bool {
        self.fuse_all
    }

    /// Record one frame's smoothed motion for a node.
    pub fn observe(&mut self, node_id: u8, smoothed_motion: f64) {
        if !self.fuse_all {
            self.stats.entry(node_id).or_default().push(smoothed_motion);
        }
    }

    /// The node room presence should follow, or `None` to fuse every fresh
    /// node (fuse-all mode, or no node has enough evidence yet).
    ///
    /// The current choice is kept until the re-rank interval passes or the
    /// node goes stale; then nodes are ranked on the evidence gathered since
    /// the last ranking, which starts over.
    pub fn select(
        &mut self,
        now: Instant,
        fresh_nodes: &[u8],
        link_scores: &BTreeMap<u8, u8>,
    ) -> Option<u8> {
        if self.fuse_all {
            return None;
        }
        let due = self.selected_at.map_or(true, |at| {
            now.saturating_duration_since(at) >= self.interval()
        });
        let selected_fresh = self.selected.is_some_and(|id| fresh_nodes.contains(&id));
        if selected_fresh && !due {
            return self.selected;
        }
        let ranking: BTreeMap<u8, RankEntry> = fresh_nodes
            .iter()
            .map(|id| {
                let h = self.stats.get(id);
                (
                    *id,
                    RankEntry {
                        contrast: h.and_then(MotionHistogram::contrast),
                        frames: h.map_or(0, MotionHistogram::frames),
                        link_score: link_scores.get(id).copied(),
                    },
                )
            })
            .collect();
        let best_contrast = ranking
            .values()
            .filter(|e| e.frames >= MIN_RANK_FRAMES)
            .filter_map(|e| e.contrast)
            .fold(None, |acc: Option<f64>, c| {
                Some(acc.map_or(c, |a| a.max(c)))
            });
        let Some(best_contrast) = best_contrast else {
            // Not enough evidence yet: keep a still-fresh choice, else fuse.
            if !selected_fresh {
                self.selected = None;
            }
            return self.selected;
        };
        self.rankings = self.rankings.saturating_add(1);
        self.selected_at = Some(now);
        self.last_ranking = ranking.clone();
        self.stats.clear();
        if best_contrast < MIN_CONTRAST {
            // Nobody was seen moving: fuse until the next ranking.
            self.selected = None;
            return None;
        }
        let choice = ranking
            .iter()
            .filter(|(_, e)| e.frames >= MIN_RANK_FRAMES)
            .filter(|(_, e)| {
                e.contrast
                    .is_some_and(|c| c * CONTRAST_TIE >= best_contrast)
            })
            .max_by(|(ia, a), (ib, b)| {
                a.link_score
                    .unwrap_or(0)
                    .cmp(&b.link_score.unwrap_or(0))
                    .then(ib.cmp(ia))
            })
            .map(|(id, _)| *id);
        self.selected = choice;
        self.selected
    }

    pub fn selected(&self) -> Option<u8> {
        self.selected
    }

    /// Time until the next ranking, once one has happened.
    pub fn next_rerank_in(&self, now: Instant) -> Option<Duration> {
        self.selected_at.map(|at| {
            self.interval()
                .saturating_sub(now.saturating_duration_since(at))
        })
    }

    pub fn last_ranking(&self) -> &BTreeMap<u8, RankEntry> {
        &self.last_ranking
    }

    /// Evidence gathered so far for the next ranking.
    pub fn pending(&self) -> BTreeMap<u8, (u64, Option<f64>)> {
        self.stats
            .iter()
            .map(|(id, h)| (*id, (h.frames(), h.contrast())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(b: &mut BestLink, id: u8, quiet: f64, active: f64, frames: u64) {
        for i in 0..frames {
            // 20% of frames active.
            b.observe(id, if i % 5 == 0 { active } else { quiet });
        }
    }

    #[test]
    fn histogram_percentiles_are_close() {
        let mut h = MotionHistogram::default();
        for i in 1..=1000 {
            h.push(i as f64 / 1000.0);
        }
        let p50 = h.percentile(0.5).unwrap();
        assert!((p50 / 0.5 - 1.0).abs() < 0.06, "{p50}");
        let p10 = h.percentile(0.1).unwrap();
        assert!((p10 / 0.1 - 1.0).abs() < 0.06, "{p10}");
    }

    #[test]
    fn the_link_that_sees_activity_wins() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        let now = Instant::now();
        feed(&mut b, 1, 0.02, 0.03, 2000); // blind: barely moves
        feed(&mut b, 2, 0.02, 0.20, 2000); // sees people
        feed(&mut b, 3, 0.10, 0.14, 2000); // noisy floor
        assert_eq!(b.select(now, &[1, 2, 3], &BTreeMap::new()), Some(2));
    }

    #[test]
    fn ties_go_to_the_better_calibrated_link() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        feed(&mut b, 1, 0.02, 0.20, 2000);
        feed(&mut b, 2, 0.02, 0.20, 2000);
        let scores = BTreeMap::from([(1, 40), (2, 80)]);
        assert_eq!(b.select(Instant::now(), &[1, 2], &scores), Some(2));
    }

    #[test]
    fn the_choice_holds_until_the_rerank_interval() {
        let mut b = BestLink::new(false, Duration::from_secs(60));
        let t0 = Instant::now();
        feed(&mut b, 1, 0.02, 0.20, 2000);
        feed(&mut b, 2, 0.02, 0.03, 2000);
        assert_eq!(b.select(t0, &[1, 2], &BTreeMap::new()), Some(1));
        // Node 2 becomes the better link, but the week is not over.
        feed(&mut b, 1, 0.02, 0.03, 2000);
        feed(&mut b, 2, 0.02, 0.20, 2000);
        assert_eq!(
            b.select(t0 + Duration::from_secs(30), &[1, 2], &BTreeMap::new()),
            Some(1)
        );
        assert_eq!(
            b.select(t0 + Duration::from_secs(61), &[1, 2], &BTreeMap::new()),
            Some(2)
        );
    }

    #[test]
    fn a_stale_choice_is_replaced_at_once() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        let t0 = Instant::now();
        feed(&mut b, 1, 0.02, 0.20, 2000);
        feed(&mut b, 2, 0.02, 0.10, 2000);
        assert_eq!(b.select(t0, &[1, 2], &BTreeMap::new()), Some(1));
        feed(&mut b, 2, 0.02, 0.10, 2000);
        assert_eq!(
            b.select(t0 + Duration::from_secs(5), &[2], &BTreeMap::new()),
            Some(2)
        );
    }

    #[test]
    fn no_clear_activity_keeps_fusing() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        feed(&mut b, 1, 0.02, 0.03, 2000);
        feed(&mut b, 2, 0.02, 0.04, 2000);
        assert_eq!(b.select(Instant::now(), &[1, 2], &BTreeMap::new()), None);
        assert_eq!(b.last_ranking().len(), 2);
    }

    #[test]
    fn early_rankings_come_sooner_than_the_weekly_one() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        let t0 = Instant::now();
        let hour = Duration::from_secs(3600);
        feed(&mut b, 1, 0.02, 0.20, 2000);
        feed(&mut b, 2, 0.02, 0.03, 2000);
        assert_eq!(b.select(t0, &[1, 2], &BTreeMap::new()), Some(1));
        assert_eq!(b.next_rerank_in(t0), Some(hour));
        feed(&mut b, 1, 0.02, 0.03, 2000);
        feed(&mut b, 2, 0.02, 0.20, 2000);
        assert_eq!(b.select(t0 + hour, &[1, 2], &BTreeMap::new()), Some(2));
        assert_eq!(b.next_rerank_in(t0 + hour), Some(4 * hour));
        let mut t = t0 + hour;
        for k in [4u32, 16, 64] {
            feed(&mut b, 2, 0.02, 0.20, 2000);
            t += hour * k;
            b.select(t, &[1, 2], &BTreeMap::new());
        }
        assert_eq!(b.next_rerank_in(t), Some(DEFAULT_RERANK));
    }

    #[test]
    fn too_little_evidence_means_fusing_all() {
        let mut b = BestLink::new(false, DEFAULT_RERANK);
        feed(&mut b, 1, 0.02, 0.20, 100);
        assert_eq!(b.select(Instant::now(), &[1], &BTreeMap::new()), None);
    }

    #[test]
    fn fuse_all_mode_never_selects() {
        let mut b = BestLink::new(true, DEFAULT_RERANK);
        feed(&mut b, 1, 0.02, 0.20, 2000);
        assert_eq!(b.select(Instant::now(), &[1], &BTreeMap::new()), None);
        assert!(b.pending().is_empty());
    }
}

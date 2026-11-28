//! Room occupancy for the Matter bridge (ADR-431 spike).
//!
//! The bridge reports one thing: whether the room is occupied. It takes that
//! from the server's own `sensing_update` broadcast, the same debounced room
//! verdict that REST and WebSocket clients see (`classification.presence`).
//!
//! This module holds no Matter SDK code, so it builds and is tested without
//! the `matter` feature. The SDK side reads [`RoomOccupancy`].
//!
//! ## Unavailable is not "vacant"
//!
//! When nothing backs the room, the bridge reports the room *unreachable*
//! rather than a frozen last value. That happens when:
//! - the fused room aggregate says `"unavailable"` (no fresh node, ADR-297);
//! - an update has no `classification.presence`;
//! - no `sensing_update` arrived for [`STALE_AFTER`].

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::broadcast;

/// How long the last `sensing_update` stays valid. The server sends several
/// per second while a source is running, so this only trips when the source
/// or the sensing loop has stopped.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

const UNAVAILABLE: u8 = 0;
const VACANT: u8 = 1;
const OCCUPIED: u8 = 2;

/// The room's state, shared between the sensing side and the bridge thread.
///
/// `None` means unavailable. Starts unavailable.
#[derive(Debug, Default)]
pub struct RoomOccupancy(AtomicU8);

impl RoomOccupancy {
    pub fn set(&self, occupied: Option<bool>) {
        let v = match occupied {
            None => UNAVAILABLE,
            Some(false) => VACANT,
            Some(true) => OCCUPIED,
        };
        self.0.store(v, Ordering::Relaxed);
    }

    pub fn get(&self) -> Option<bool> {
        match self.0.load(Ordering::Relaxed) {
            VACANT => Some(false),
            OCCUPIED => Some(true),
            _ => None,
        }
    }
}

/// Turns the server's broadcast messages into the room's occupancy.
#[derive(Debug, Default)]
pub struct OccupancyFeed {
    /// When the last `sensing_update` arrived, and what it said.
    last: Option<(Duration, Option<bool>)>,
}

impl OccupancyFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one broadcast message. Messages other than `sensing_update` are
    /// ignored. `now` is a monotonic time, for example since the feed started.
    pub fn ingest(&mut self, v: &Value, now: Duration) {
        if v["type"].as_str() != Some("sensing_update") {
            return;
        }
        self.last = Some((now, occupancy_of(v)));
    }

    /// The room's occupancy at `now`. `None` if unavailable or stale.
    pub fn state(&self, now: Duration) -> Option<bool> {
        let (at, occupied) = self.last?;
        if now.saturating_sub(at) >= STALE_AFTER {
            return None;
        }
        occupied
    }
}

/// How often [`spawn_feed`] re-checks staleness when no message arrives.
const FEED_TICK: Duration = Duration::from_millis(500);

/// Follow the server's JSON broadcast and keep `room` up to date, until the
/// broadcast closes. Call from inside the tokio runtime.
pub fn spawn_feed(
    mut rx: broadcast::Receiver<String>,
    room: Arc<RoomOccupancy>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started = Instant::now();
        let mut feed = OccupancyFeed::new();
        let mut tick = tokio::time::interval(FEED_TICK);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(json) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&json) {
                            feed.ingest(&v, started.elapsed());
                        }
                    }
                    // Missed messages: the next update carries the full state.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => {}
            }
            room.set(feed.state(started.elapsed()));
        }
        room.set(None);
    })
}

/// The occupancy one `sensing_update` reports, or `None` if it can't say.
fn occupancy_of(v: &Value) -> Option<bool> {
    if v["room_inference"]["classification"].as_str() == Some("unavailable") {
        return None;
    }
    v["classification"]["presence"].as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(presence: bool) -> Value {
        json!({
            "type": "sensing_update",
            "classification": {"presence": presence, "motion_level": "active", "confidence": 0.8},
        })
    }

    const T0: Duration = Duration::from_secs(100);

    #[test]
    fn room_occupancy_round_trips_and_starts_unavailable() {
        let room = RoomOccupancy::default();
        assert_eq!(room.get(), None);
        for v in [Some(true), Some(false), None] {
            room.set(v);
            assert_eq!(room.get(), v);
        }
    }

    #[test]
    fn nothing_received_is_unavailable() {
        assert_eq!(OccupancyFeed::new().state(T0), None);
    }

    #[test]
    fn follows_the_room_presence() {
        let mut feed = OccupancyFeed::new();
        feed.ingest(&update(true), T0);
        assert_eq!(feed.state(T0), Some(true));
        feed.ingest(&update(false), T0 + Duration::from_millis(200));
        assert_eq!(feed.state(T0 + Duration::from_millis(200)), Some(false));
    }

    #[test]
    fn unavailable_room_aggregate_is_not_vacant() {
        let mut feed = OccupancyFeed::new();
        let mut v = update(false);
        v["room_inference"] =
            json!({"classification": "unavailable", "confidence": 0.0, "contributing_nodes": 0});
        feed.ingest(&v, T0);
        assert_eq!(feed.state(T0), None);
    }

    #[test]
    fn available_room_aggregate_uses_the_debounced_presence() {
        let mut feed = OccupancyFeed::new();
        let mut v = update(true);
        v["room_inference"] =
            json!({"classification": "present_still", "confidence": 0.7, "contributing_nodes": 2});
        feed.ingest(&v, T0);
        assert_eq!(feed.state(T0), Some(true));
    }

    #[test]
    fn missing_presence_is_unavailable() {
        let mut feed = OccupancyFeed::new();
        feed.ingest(&json!({"type": "sensing_update"}), T0);
        assert_eq!(feed.state(T0), None);
    }

    #[test]
    fn other_message_types_are_ignored() {
        let mut feed = OccupancyFeed::new();
        feed.ingest(&update(true), T0);
        feed.ingest(
            &json!({"type": "edge_vitals", "classification": {"presence": false}}),
            T0 + Duration::from_secs(1),
        );
        assert_eq!(feed.state(T0 + Duration::from_secs(1)), Some(true));
    }

    /// The broadcast path end to end: JSON strings in, `RoomOccupancy` out,
    /// through a lag and until the channel closes.
    #[tokio::test]
    async fn feed_task_follows_the_broadcast() {
        async fn wait_for(room: &RoomOccupancy, want: Option<bool>) {
            for _ in 0..200 {
                if room.get() == want {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("room never became {want:?} (is {:?})", room.get());
        }

        let (tx, rx) = broadcast::channel::<String>(4);
        let room = Arc::new(RoomOccupancy::default());
        let task = spawn_feed(rx, room.clone());

        tx.send(update(true).to_string()).unwrap();
        wait_for(&room, Some(true)).await;
        tx.send("not json".into()).unwrap();
        tx.send(update(false).to_string()).unwrap();
        wait_for(&room, Some(false)).await;

        // Overflow the channel: the task lags, then keeps going.
        for _ in 0..10 {
            tx.send(update(true).to_string()).unwrap();
        }
        wait_for(&room, Some(true)).await;

        drop(tx);
        task.await.unwrap();
        assert_eq!(room.get(), None, "a closed broadcast is unavailable");
    }

    #[test]
    fn goes_unavailable_when_updates_stop() {
        let mut feed = OccupancyFeed::new();
        feed.ingest(&update(true), T0);
        assert_eq!(
            feed.state(T0 + STALE_AFTER - Duration::from_millis(1)),
            Some(true)
        );
        assert_eq!(feed.state(T0 + STALE_AFTER), None);
        // A fresh update brings it back.
        feed.ingest(&update(true), T0 + STALE_AFTER + Duration::from_secs(1));
        assert_eq!(
            feed.state(T0 + STALE_AFTER + Duration::from_secs(1)),
            Some(true)
        );
    }
}

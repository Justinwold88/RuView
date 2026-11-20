//! Vital sign detection from WiFi CSI data.
//!
//! The detector lives in the `wifi-densepose-vitals` crate (plan item 4.1,
//! review R11), so the server and the crate share one implementation. This
//! module re-exports it unchanged, so `crate::vital_signs::VitalSignDetector`
//! and `wifi_densepose_sensing_server::vital_signs` keep working.

pub use wifi_densepose_vitals::detector::*;

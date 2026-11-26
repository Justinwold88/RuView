- **Home Assistant: each node's link score as a diagnostic sensor.** After a
  node's empty-room calibration (`POST /api/v1/calibration/motion/start`), its
  0–100 link score now rides on the node's `node_features` entry in the
  sensing broadcast. Over MQTT it appears as a `Link score` sensor in the
  device's Diagnostic section. It is unavailable until the node is calibrated,
  published in privacy mode too (it describes the link, not a person), and not
  exposed over Matter. Its weights are SYNTHETIC-tuned. Part of issue #52.

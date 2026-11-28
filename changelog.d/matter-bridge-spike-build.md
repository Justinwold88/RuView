- **Matter bridge spike builds and starts (issue #50, ADR-431).** With the
  `matter` cargo feature, `sensing-server --matter` runs a presence-only
  bridge on rs-matter 0.5.0: an Aggregator and one occupancy sensor for the
  room, which follows the debounced room verdict and reports unreachable when
  no fresh source backs it. The `--matter*` flags now reach the binary (it
  used to reject `--matter`), and `--matter-setup-file` writes the test setup
  codes, and `--matter-test-toggle-secs` flips occupancy on a SYNTHETIC
  schedule for pairing tests without sensing hardware. It uses the CSA test
  vendor ID and test credentials. rs-matter 0.5
  needs Rust 1.92, so the feature builds with `cargo +stable`, and a new CI
  job (`matter-bridge.yml`) tests it. No controller has paired with it yet:
  the development sandbox has no IPv6, so pairing needs a real network.
  Docs no longer say the bridge can't be started, and the Home Assistant
  guide no longer lists chip-tool as tested.

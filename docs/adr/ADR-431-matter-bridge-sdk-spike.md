# ADR-431: Matter bridge: try rs-matter in a presence-only spike before choosing the SDK (refines ADR-115 §3.11.3)

- **Status**: proposed (fork; needs the maintainer's decision). Spike in progress: the bridge builds and starts; no controller has paired with it yet (see [Spike log](#spike-log)).
- **Date**: 2026-11-27
- **Deciders**: RuView maintainers; fork owner
- **Tags**: matter, smart-home, alexa, bridge
- **Issue**: [#50](https://github.com/Justinwold88/RuView/issues/50)

## Context

[ADR-115 §3.11](ADR-115-home-assistant-integration.md) designs a Matter bridge
inside `wifi-densepose-sensing-server`: a Bridged Devices Aggregator with
one occupancy endpoint per room, and fall events as a generic switch. Heart
rate, breathing rate and pose are never exposed. The type and cluster mapping
exists (`src/matter/`), but the `matter` cargo feature is empty, so
`--matter` can't start anything. ADR-115 §3.11.3 left the SDK open, with
`rs-matter` as the tentative choice "deferred to P7 spike". That spike never
ran.

Today Alexa, Apple Home and Google Home reach RuView through Home Assistant:
the motion-class template sensor for Alexa routines (#42), or the community
Home-Assistant-Matter-Hub add-on (#65). Both need Home Assistant running. A
bridge in RuView itself would give a hub-free, subscription-free path.

### What changed since ADR-115 (checked 2026-11-27)

- **rs-matter 0.3.0** was published on 2026-08-20. Its documentation
  describes it as a pure-Rust, `no_std`, async implementation of Matter 1.6
  that runs on std Linux (UDP sockets, `async-io`) as well as MCUs.
  - It lists Google Home, Apple HomeKit, Alexa, Home Assistant, SmartThings
    and IKEA Dirigera as controllers it has been tested with.
  - It is **not certified**, and its API is not yet stable.
  - It is licensed **Apache-2.0**. ADR-115 §3.11.3 says MIT, which is wrong.
  - Its published example is a single-endpoint device. Whether it supports an
    aggregator with bridged endpoints and an `OccupancySensing` server, as a
    library user would build them, is **not confirmed** from its
    documentation.
- **matter.js** (Node.js) is the stack under both Home-Assistant-Matter-Hub
  and **Matterbridge**. Matterbridge is a plugin manager that exposes plugin
  devices as one Matter bridge to Apple Home, Google Home, Alexa and
  SmartThings, and it has production plugins for zigbee2mqtt and Shelly.
  A RuView plugin would be a separate Node process.
- **Alexa and test vendor IDs** (issue #50's open question).
  - Amazon's Alexa Connect Kit guide commissions prototype Matter devices
    that use the CSA test vendor ID (0xFFF1) with test credentials.
  - It requires the Echo and the Alexa app to be signed in to the same
    Amazon developer account, which is free.
  - One public report (STM32 forum, an Echo 4th Gen) describes the Alexa app
    not discovering a 0xFFF1 Thread device over BLE.
  - So it is **likely but unconfirmed** that an Echo will commission a
    test-VID bridge over Wi-Fi/IP. Only a real Echo can settle it.

## Decision (proposed)

Run a **time-boxed spike** with `rs-matter` before committing to an SDK. The
spike is the "first step" of issue #50 and decides between two outcomes.

**Spike scope**: behind the existing `matter` feature, add `rs-matter` and
start a bridge that exposes one Aggregator with one `OccupancySensor`
endpoint per room (`OccupancySensing.Occupancy` from the room verdict).
- Fall events, the vendor person-count attribute and zones stay out.
- It uses the CSA test vendor ID, test PAA/DAC and a printed setup code. No
  production credentials.
- `--privacy-mode` still applies. Occupancy is not biometric, so the endpoint
  stays.
- It has its own UDP port, and on-network commissioning only (no BLE).

**Pass criteria**, all with captured logs. A build or a simulator is not
enough.
1. Commissioned by **chip-tool** or **Home Assistant's Matter integration**.
   Occupancy changes reach the controller within 2 s.
2. Commissioned by the fork owner's **Echo Show** through the Alexa app on an
   Amazon developer account. An occupancy change triggers an Alexa routine.
3. A second commissioning (multi-admin) doesn't break the first.

**Outcomes**
- **Pass**: write the full bridge on rs-matter, as ADR-115 §3.11 describes,
  in later PRs, and record that here.
- **Fail on bridge support** (no workable aggregator or occupancy server in
  rs-matter): drop the in-process bridge, and build a **Matterbridge plugin**
  that reads RuView's existing MQTT discovery or REST state instead. It is a
  separate process on matter.js, which ADR-115 §3.11.3 listed as option 3.
- **Fail on Alexa only** (other controllers work, Echo refuses a test-VID
  device): ship for Home Assistant, Apple Home and Google Home. Alexa stays on
  the Home Assistant routes (#42, #65) until a vendor ID is obtained.
- `connectedhomeip` through FFI stays the last resort. Its C++/CMake build and
  vendored size conflict with the Rust-only crate policy.

## Consequences

- The spike needs the **fork owner's hardware and network**: the Echo Show and
  the always-on Windows PC.
  - Matter needs mDNS and IPv6 on the LAN.
  - Under WSL 2's default NAT networking, multicast doesn't reach the LAN.
    The bridge then has to run natively on Windows, in WSL's mirrored
    networking mode, or in the Hyper-V VM on a bridged switch planned for
    Home Assistant OS.
- No claim that RuView "works with Matter" or "works with Alexa without Home
  Assistant" until criteria 1 and 2 have logs. The README says "Matter:
  planned" (#71).
- A Matter bridge on test credentials is for development. Shipping one to
  other people needs a CSA vendor ID and certification, which is out of scope.
- Heart rate, breathing rate, pose and RSSI stay MQTT-only, as ADR-115 says.

## Spike log

### 2026-11-27: the bridge builds and starts

**SDK version.** rs-matter **0.5.0** was released on 2026-11-27. It is used instead of 0.3.0 for two reasons:
- Its repository has a bridge example: an Aggregator with Bridged Node endpoints, each with a `BridgedDeviceBasicInformation` server. That settles the "not confirmed" point above at the API level.
- Its `MatterStack` runs an Ethernet/IP node in a few calls.

The cost is that 0.5 needs **Rust 1.92**, and the workspace pins 1.89 (`v2/rust-toolchain.toml`). So the `matter` feature is built with `cargo +stable`, and CI runs it in its own job on stable (`.github/workflows/matter-bridge.yml`). The default build and the pin are unchanged. rs-matter 0.4.1 (Rust 1.87) also has the bridge example, but needs more hand-written transport and mDNS code. It is the fallback if the toolchain override becomes a problem.

**Shape.** In process, behind the `matter` feature, as decided above.
- `src/matter/runtime.rs` runs the Matter stack on its own thread with its own executor, so it never blocks the server's tokio runtime.
- `src/matter/occupancy.rs` follows the server's own `sensing_update` broadcast, the same one the MQTT bridge reads. It shares one atomic room state with the Matter handlers, which poll it every 100 ms and notify subscribers on a change.
- `src/matter/config.rs` checks the flags.

**Endpoints.**
- 0: root node.
- 1: Aggregator.
- 2: the room. Device types Occupancy Sensor (0x0107) + Bridged Node (0x0013). Clusters: Descriptor, Identify, Bridged Device Basic Information, and Occupancy Sensing.
  - Occupancy Sensing advertises the `RFSensing` feature. The legacy sensor-type attributes say PIR, as the spec asks for sensors with none of the legacy modalities.
  - Bridged Device Basic Information has `Reachable`, `UniqueID` and a writable `NodeLabel`.

This replaces the tree drawn in `src/matter/bridge.rs` for the spike: that tree put the Aggregator on endpoint 0, which Matter reserves for the root node, and gave bridged endpoints Basic Information instead of Bridged Device Basic Information. `bridge.rs` is not wired to the SDK and is left for the full bridge.

**Occupancy source.** The debounced room verdict (`classification.presence`, the value REST and WebSocket report). The room reports **unreachable**, and unoccupied, when:
- the fused room aggregate says `unavailable` (ADR-297);
- an update has no presence;
- no update arrived for 10 s.

**Flags.** `--matter`, `--matter-setup-file`, `--matter-reset`, `--matter-vendor-id` and `--matter-product-id` now reach the binary. A new `--matter-test-toggle-secs N` flips occupancy every N seconds and ignores sensing. It's a SYNTHETIC source for running the pass criteria before any sensing hardware exists: the simulated source's room stays occupied, so it can't trigger a routine. Before this, they existed only on the library's unused `cli::Args`, and the binary rejected `--matter`. Any vendor/product ID other than the bundled test attestation's `0xFFF1`/`0x8001` is refused at start, because it would fail device attestation.

**Checked so far** (no controller involved):
- Unit tests, which run in the new CI job:
  - the occupancy feed, including staleness, lag and a closed broadcast;
  - flag checks;
  - the cluster metadata: RF-sensing feature, required attributes, and the endpoint and device-type layout;
  - the setup codes: the manual code is the standard test code `3497-011-2332`.
- A startup run with `--source simulated --matter` in the development sandbox:
  - the commissioning window opens;
  - the QR text and pairing code are logged, and written to the setup file with mode 0600;
  - the stack then stops at the UDP bind, because the sandbox has **no IPv6** (`Address family not supported by protocol`), and the rest of the server keeps running.
- So **commissioning could not be tried there**. That and every pass criterion above still need the fork owner's network, with captured logs.
- `cargo audit` reports no new advisories or warnings from the 83 crates the optional dependency adds to `Cargo.lock`.
  - Their licenses are permissive (Apache-2.0, MIT, BSD-3-Clause or ISC, alone or as a choice).
  - The exceptions are three MPL-2.0 build-time crates of rs-matter's code generator. The lock already had MPL-2.0 crates.

## Alternatives considered

| Option | For | Against |
|---|---|---|
| rs-matter in process (this ADR's spike) | Pure Rust, fits the crate policy, Matter 1.6, lists Alexa as tested | Not certified; API unstable; bridge support unconfirmed |
| Matterbridge plugin (matter.js) | Mature bridge with production plugins; no Matter code in RuView | Node.js runtime and a second process; another dependency to keep current |
| Home-Assistant-Matter-Hub (#65) | Works today, no RuView code | Needs Home Assistant; untested with RuView |
| connectedhomeip through FFI | Reference implementation | C++ toolchain, CMake, large vendored code |

## Sources

- rs-matter 0.3.0 documentation: <https://docs.rs/crate/rs-matter/0.3.0>
- rs-matter 0.5.0 bridge example: <https://github.com/project-chip/rs-matter/blob/v0.5.0/examples/src/bin/bridge.rs>
- Amazon, ACK Matter tutorial, step 3 (commission with an Echo on a developer
  account): <https://developer.amazon.com/docs/alexaplus/ack/matter-tutorial-test-device.html>
- Amazon, provisioning ACK Matter prototypes with test VID 0xFFF1:
  <https://developer.amazon.com/en-US/docs/alexa/ack/matter-provision-device.html>
- STM32 community report of an Echo 4th Gen not discovering a 0xFFF1 device:
  <https://community.st.com/stm32-mcus-wireless-35/stm32wba65-x-cube-matter-alexa-app-does-not-detect-matter-device-during-ble-commissioning-166774>
- Matterbridge: <https://github.com/Luligu/matterbridge>

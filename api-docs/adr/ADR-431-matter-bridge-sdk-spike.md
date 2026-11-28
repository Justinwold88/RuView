# ADR-431: Matter bridge: try rs-matter in a presence-only spike before choosing the SDK (refines ADR-115 §3.11.3)

- **Status**: proposed (fork; needs the maintainer's decision)
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

## Alternatives considered

| Option | For | Against |
|---|---|---|
| rs-matter in process (this ADR's spike) | Pure Rust, fits the crate policy, Matter 1.6, lists Alexa as tested | Not certified; API unstable; bridge support unconfirmed |
| Matterbridge plugin (matter.js) | Mature bridge with production plugins; no Matter code in RuView | Node.js runtime and a second process; another dependency to keep current |
| Home-Assistant-Matter-Hub (#65) | Works today, no RuView code | Needs Home Assistant; untested with RuView |
| connectedhomeip through FFI | Reference implementation | C++ toolchain, CMake, large vendored code |

## Sources

- rs-matter 0.3.0 documentation: <https://docs.rs/crate/rs-matter/0.3.0>
- Amazon, ACK Matter tutorial, step 3 (commission with an Echo on a developer
  account): <https://developer.amazon.com/docs/alexaplus/ack/matter-tutorial-test-device.html>
- Amazon, provisioning ACK Matter prototypes with test VID 0xFFF1:
  <https://developer.amazon.com/en-US/docs/alexa/ack/matter-provision-device.html>
- STM32 community report of an Echo 4th Gen not discovering a 0xFFF1 device:
  <https://community.st.com/stm32-mcus-wireless-35/stm32wba65-x-cube-matter-alexa-app-does-not-detect-matter-device-during-ble-commissioning-166774>
- Matterbridge: <https://github.com/Luligu/matterbridge>

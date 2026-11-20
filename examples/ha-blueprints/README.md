# RuView starter Home Assistant Blueprints

9 ready-to-import HA Blueprints. Eight cover the highest-leverage automations
RuView's HA-MIND semantic primitives unlock; the ninth fuses RuView with
mmWave and BLE sensors you already have in Home Assistant. Drop the YAML
files into `<HA config>/blueprints/automation/ruvnet/` and import from the HA
UI (**Settings → Automations & Scenes → Blueprints → Import Blueprint**).

| # | Blueprint                                                           | Primary primitive            | Use case                              | Published today? |
|---|---------------------------------------------------------------------|------------------------------|---------------------------------------|------------------|
| 1 | [Notify on possible distress](01-notify-on-possible-distress.yaml)   | `possible_distress`          | Healthcare / AAL / single-occupant    | ✅ |
| 2 | [Dim hallway when sleeping](02-dim-hallway-when-sleeping.yaml)       | `someone_sleeping`           | Convenience / sleep hygiene           | ✅ |
| 3 | [Wake routine on bed exit](03-wake-routine-on-bed-exit.yaml)         | `bed_exit`                   | Morning routine / smart home          | ❌ not yet: imports, never fires |
| 4 | [Alert on elderly inactivity anomaly](04-alert-elderly-inactivity-anomaly.yaml) | `elderly_inactivity_anomaly` | AAL / aging-in-place           | ✅ |
| 5 | [Meeting lights + presence mode](05-meeting-lights-presence-mode.yaml) | `meeting_in_progress`      | Conference room / WFH                 | ❌ not yet: use any binary_sensor |
| 6 | [Bathroom fan while occupied](06-bathroom-fan-while-occupied.yaml)   | `bathroom_occupied`          | Humidity / privacy-mode-safe          | ❌ not yet: use the bathroom node's `presence` |
| 7 | [Escalate on fall-risk crossing](07-fall-risk-escalation.yaml)       | `fall_risk_elevated`         | AAL / preventive intervention         | ✅ |
| 8 | [Auto-arm security when room not active](08-auto-arm-security-when-not-active.yaml) | `room_active` + `no_movement` | Self-arming security | ✅ |
| 9 | [Companion fusion: Wi-Fi + mmWave + BLE](09-companion-fusion-occupancy.yaml) | `presence` + your mmWave and BLE entities | One "room occupied" helper from several sensors | ✅ |

"Published today?" means the RuView server's MQTT discovery planner has a
source for the primitive. `bed_exit`, `meeting_in_progress` and
`bathroom_occupied` don't yet, so their entities stay unavailable. Each of
those blueprints says so in its description. ✅ is conditional too: every
inferred primitive waits for the planner's warm-up, and 01, 02 and 07 are
available only while heart rate, breathing rate and fall detection
(respectively) are fresh. Separately,
`scripts/validate-ha-blueprints.py` reads the planner and fails if a
blueprint and the server disagree.

## Verifying the YAML

Each blueprint validates against the HA blueprint schema
(https://www.home-assistant.io/docs/blueprint/schema/). To check locally
without an HA install:

```bash
# Requires python3 + PyYAML; run from the repository root
python scripts/validate-ha-blueprints.py
```

## Companion fusion (09)

Blueprint 09 (plan item 3.3) drives one toggle helper, for example
`input_boolean.living_room_occupied`, from up to three sources:

| Source | What it is good at | Weak spot |
|---|---|---|
| RuView `presence` | Sees the whole room; catches walk-ins fast | A still person can drop out; fans and pets can cause false events |
| mmWave presence (e.g. ESPHome HLK-LD2410, Seeed MR60BHA2) | Holds people who sit still inside its cone | Narrow field of view |
| BLE room presence (e.g. Bermuda, ESPresense) | Knows a tracked phone or tag is in the room | Usually slower to update; a phone can be left behind |

- **Start:** any source reporting present starts occupancy, so lights stay
  responsive. Turn on **Wi-Fi needs confirmation** where a false "occupied"
  costs more than a late one (alarms, notifications). RuView's presence
  alone then cannot start occupancy, but it holds occupancy once mmWave or
  BLE has seen someone.
- **Clear:** the room clears only after every source has been clear for the
  clear delay (default 60 s). Unavailable sources count as clear. After a
  Home Assistant restart, a helper left on clears if nothing reports present
  for the clear delay.
- **Privacy:** the fusion runs entirely in Home Assistant. RuView never
  receives the mmWave or BLE data and stores no BLE identity.
- **Tested** against Home Assistant core 2026.10.4 with simulated state
  changes (SYNTHETIC): `examples/ha-blueprints/tests/`, run in CI by
  `ha-blueprint-tests.yml`. It has not yet been run with a real mmWave
  module or BLE tracker.

```bash
python3.14 -m pip install -r examples/ha-blueprints/tests/requirements.txt
cd examples/ha-blueprints/tests && python3.14 -m pytest -q
```

## Privacy-mode compatibility

Six of the nine blueprints work under `--privacy-mode` (no biometrics
exposed). The other three depend on inferred states that themselves
derive from biometrics, so they still publish, but the operator should
audit before deploying in regulated contexts.

| Blueprint                                | Privacy-mode safe? |
|------------------------------------------|--------------------|
| 01 Notify on possible distress           | ⚠️ derives from HR/motion — needs `--mqtt-publish-heart-rate`; state still publishes |
| 02 Dim hallway when sleeping             | ⚠️ derives from BR — state still publishes |
| 03 Wake routine on bed exit              | ✅                  |
| 04 Alert on elderly inactivity anomaly   | ✅                  |
| 05 Meeting lights                        | ✅                  |
| 06 Bathroom fan while occupied           | ✅ zone-derived only |
| 07 Escalate on fall-risk crossing        | ⚠️ derives from motion-variance — state still publishes |
| 08 Auto-arm security                     | ✅                  |
| 09 Companion fusion                      | ✅ presence only; mmWave and BLE stay in HA |

The "⚠️" markers are the inferred-state-vs-raw-value distinction from
[ADR-115 §3.12.3](../../docs/adr/ADR-115-home-assistant-integration.md#3123-why-these-specific-primitives):
the *state* (e.g. `binary_sensor.someone_sleeping`) crosses the wire
even in privacy mode because it's derived server-side, but it's no
longer accompanied by the raw biometric values.

## See also

- [ADR-115](../../docs/adr/ADR-115-home-assistant-integration.md) — full design
- [`docs/integrations/home-assistant.md`](../../docs/integrations/home-assistant.md) — operator guide
- [`docs/integrations/semantic-primitives-metrics.md`](../../docs/integrations/semantic-primitives-metrics.md) — per-primitive F1

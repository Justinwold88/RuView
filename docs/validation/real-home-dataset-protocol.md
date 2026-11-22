# Real-home dataset protocol (plan item 2.3)

**Status: PROPOSED. It needs the study owner's sign-off before the first
recording day.** After sign-off, the sections marked *frozen* do not change
for this dataset. A change means a new protocol version and a new dataset.

This protocol produces the labelled real-home recordings that plan item 2.4
uses to report Gate G2 and the Phase 1 metrics as **MEASURED**. Later items
reuse it too: 2.5 (sensing area), 3.2 (pets and robot vacuums) and 5.3
(fine-tuning). Until a run under this protocol exists, every RuView accuracy
number is SYNTHETIC or CLAIMED.

## What it measures (frozen)

Gate G2, as accepted in the plan, with these definitions:

| Metric | Definition | G2 target |
|---|---|---|
| False occupied events per empty-home day | In `empty_home` intervals (minus guard bands), count each change of the room's reported presence from absent to present. Changes less than 60 s apart count as one event. Divide by the guarded empty-home hours, then multiply by 24. | ≤ 1 |
| Still person held | The share of guarded `still` minutes, at spots inside the best link's sensing area, for which the room reports present | ≥ 90% |
| Walk-in latency | The time from each `walk_in` reference to the room's first present report. A walk-in counts as missed after 10 s. | Every walk-in ≤ 2 s; report the median, p90 and misses |

Reported alongside, but not part of G2:

- false occupied events per hour while the study room is empty but people are
  elsewhere in the home (`room_empty`)
- still minutes held at spots outside the sensing area
- presence held while asleep
- the share of `pet_only` episodes that raise occupied (input for 3.2)
- false occupied events per hour while nobody is home and pets are loose
  (`pets_alone`, input for 3.2)
- Phase 1 metrics on real data (item 2.4):
  - breathing rate against the mmWave reference, where the mmWave module reports one
  - frames per second
  - processing time per frame

**Guard bands.** Labels can be off by a few seconds, and the server holds
presence for up to about 50 s after a seated person leaves (SYNTHETIC, item
1.3). To allow for both:

- The first 120 s and the last 30 s of every `empty_home` and `room_empty`
  interval are excluded.
- So are the first and last 30 s of every `still` and `sleep` interval.

**Minimum test data.** G2 is reported only if the test days hold all of these.
Otherwise the result is "insufficient data", with what is missing.

- at least 24 h of guarded `empty_home` time
- at least 120 guarded `still` minutes inside the sensing area
- at least 30 `walk_in` events with a reference time

### Scoring details (frozen)

- **Room verdict.** The scored verdict is the room's `classification.presence`
  from the output log.
  - Each logged value holds until the next one.
  - A gap of more than 5 s between logged updates counts as no output. That
    covers a stopped, stale or unreachable server.
  - No-output time is removed from every denominator and reported as coverage.
- **False occupied event.** Within a guarded `empty_home` piece, an event
  starts in either of two cases:
  - the verdict is already present when the piece starts
  - the verdict turns from absent to present

  A present spell that starts less than 60 s after the previous one ended, in
  the same piece, belongs to the same event.
- **Still held.** Present time divided by guarded `still` time, after removing
  no-output time.
- **Walk-in latency.** The window runs from 2 s before the reference to 10 s
  after it.
  - **Already present:** the verdict is present 2 s before the reference. This
    is a false positive. It is reported separately and fails the 2 s target.
  - **Otherwise:** the latency is the first absent-to-present change in the
    window, minus the reference. It can be negative, down to −2 s, since a
    button press can lag.
  - **Missed:** there is no change in the window.
- **Breathing (Phase 1 check).**
  - Over guarded `still` and `sleep` pieces, compare each logged
    `breathing_bpm` with the mmWave breathing rate in the fused vitals packets
    of the tee capture: magic `0xC5110004`, float at bytes 32–35, used only
    when flags bit 3 is set and the rate is 6–40 BPM.
  - Use the latest mmWave value from the last 5 s. Report the median absolute
    error.
  - The mmWave module is a reference, not ground truth. Its own accuracy is
    CLAIMED by its maker.

## Consent and privacy (frozen)

CSI recordings are person data. The repository rules forbid committing them,
and this protocol forbids uploading them anywhere.

- **Consent.** Every adult who lives in the home, or stays there overnight
  during the study, agrees in writing before recording starts. They can stop
  the study, or have their data deleted, at any time. A child is recorded only
  with a parent's consent, and their labels say only "person", never an age.
- **Visitors.** When a visitor who has not consented is in the home, label the
  time `exclude` or stop the recorder. Excluded time is deleted before
  analysis.
- **No cameras or microphones.** Ground truth comes from labels, the mmWave
  module on a node, and optionally a door contact sensor.
- **Labels carry no names.** Use `P1`, `P2` and so on. Notes must not contain
  names, health details or anything said in the home.
- **Storage.** Raw captures and labels stay in one private directory outside
  any Git checkout, on an encrypted disk, on the study owner's machine.
  `record_tee.py` refuses an output directory inside a Git work tree.
- **What may be published:** aggregate numbers and the scripts that produce
  them. Raw captures, labels and per-minute outputs are never published.
- **Retention.** Raw captures are deleted 12 months after the last recording
  day unless everyone renews consent. Aggregate results may be kept.

## Setup sheet (filled in at recording time, kept with the data)

Record each of these once. If any changes, record it again with the time.
They describe the home, so they stay private with the data.

- study room, its size, the doorway(s) used for walk-ins, and how many people
  and pets live in the home
- where the pets are shut in during `home_empty`, and confirmation that this
  room has no node and is outside every link's sensing area
- each node's board, firmware version and app SHA-256, position and height,
  plus the access point position, channel and bandwidth
- the mmWave module type and position, if one is fitted
- the marked still spots, numbered S1, S2 and so on, and whether each is
  inside the best link's sensing area. That is the area where a walk-in
  through it is caught on a spot test (item 2.5).
- the sensing-server version and command line, and the calibration result
  (`GET /api/v1/calibration/motion`)
- fans, heaters, curtains or robot vacuums in or next to the room

**Hardware evidence.** Before day 1, capture each board's boot log and 5
minutes of runtime log (item 2.1). A build or a simulator is not evidence.

## Recording (frozen)

1. **Start the server with a tee.** Start the sensing server as usual, with
   `--udp-tee 127.0.0.1:5600`. The tee copies every admitted datagram to the
   recorder unchanged, including mmWave and sync packets.
2. **Run the recorder for the whole study.**

   ```bash
   python tools/real_home/record_tee.py --listen 127.0.0.1:5600 --output /private/ruview-home
   ```

   It writes one nanosecond pcap per UTC hour (link type 147, the datagram
   bytes unchanged). Each file is closed with its SHA-256 and per-type counts in
   `manifest.jsonl`. It prints packet rates every 10 minutes, so a dead node is
   noticed on the same day.
3. **Log the live verdict.** Run the output logger for the whole study too. It
   polls `GET /api/v1/sensing/latest` every 0.25 s and `GET /api/v1/links`
   every minute, and keeps only the room verdict, each node's level, the
   followed link and the breathing estimate:

   ```bash
   python tools/real_home/log_outputs.py --url http://127.0.0.1:8080 --output /private/ruview-home
   ```

   G2 is scored on this log: it is what Home Assistant saw. The raw tee capture
   allows later replays with other settings.
4. **Calibrate.** Run the empty-room calibration (item 1.1) on day 1, and again
   after anything in the setup sheet changes.
5. **Duration.** Record for at least 7 consecutive days, around the clock.
   The host clock must be NTP-synchronised. The recorder logs clock jumps above
   2 s.

## Labels (frozen)

Labels are timestamped events in one CSV file per study, appended with
`tools/real_home/labels.py mark`. The host clock is the time reference.
Labels made on a phone or in Home Assistant need an NTP-synchronised clock,
and are copied into the CSV with `--at`.

| Event | Meaning |
|---|---|
| `home_empty_start` / `home_empty_end` | Nobody is in the home and no pets are loose: every pet is out of the home or shut in a closed room that has no node and is outside every link's sensing area. Log the time the last person leaves, then the time the first person returns. |
| `pets_alone_start` / `pets_alone_end` | Nobody is in the home and pets are loose. Logged like `home_empty`. Never counted as `home_empty`. |
| `room_empty_start` / `room_empty_end` | Nobody is in the study room, but people are elsewhere in the home |
| `still_start` / `still_end` | One person sits or lies awake at a marked spot and barely moves: reading, a laptop, TV. They start once seated. Use `--spot S1`, or `--spot S4-out` for a spot outside the sensing area. |
| `sleep_start` / `sleep_end` | A person in the room is asleep (lights out until getting up) |
| `walk_in` | The moment a person crosses the doorway into an empty study room. `--source door` uses a contact or doorway sensor. `--source button` is a press while crossing. `--source mmwave` is checked later against mmWave onset. Marking a walk-in also ends the room's open `room_empty` at the same moment. Only walk-ins after at least 2 minutes of `room_empty` count. |
| `pet_only_start` / `pet_only_end` | Pets are in the room with no people |
| `exclude_start` / `exclude_end` | Visitors who have not consented, cleaning, setup changes, or anything unusual. Excluded from every metric. |

Any time without a label is "unlabelled". It is never used for G2.

**Planned sessions.** Free living rarely gives clean labels, so each day
includes:

- **two still sessions** of 20 to 30 minutes at marked spots, at least one
  inside the sensing area
- **one walk-in session:** ten walk-ins at least 60 s apart, each from at
  least 2 minutes of `room_empty`, with the doorway sensor or button as the
  reference
- **every absence from home**, logged as `home_empty`

The pet episodes and sleep are labelled as they happen.

**Homes with pets.** Wi-Fi sensing and the mmWave module both respond to
animals, so what was in the room decides how time counts:

- G2's empty-home metric needs people and pets absent. In a home with pets,
  `home_empty` time therefore needs the pets shut in, as defined in the label
  table. If the test days don't reach 24 guarded `home_empty` hours, G2 is
  reported as "insufficient data". It is never filled with `pets_alone` time.
- `pets_alone` time is reported separately, as false occupied events per hour.
  It shows how often pets alone make the room read as occupied, which is the
  question for plan item 3.2.
- `pet_only` episodes are labelled only when someone sees them, for example
  from the doorway. Unlabelled pet time is never used.
- Pets stay out of the room during fall sessions (see the
  [fall study](fall-study-protocol.md)).

**Reference checks, reported and not fixed silently:**

- **Walk-in reference.** Where a node reports mmWave presence, its onset should
  fall within 2 s of the `walk_in` reference. Walk-ins whose references
  disagree by more than 2 s are reported as such. They are kept in the count
  and are not corrected.
- **Empty home.** If the mmWave module reports a target during `empty_home`,
  the interval is reported as contradicted. Whether it still counts is decided
  before the test days are opened (see Split).

## Split by day (frozen)

- **Days.** A day runs from 04:00 to 04:00 local time, so a night falls within
  one day. Day 1 is the first day with labels.
- **Train days (1–3).** Used for setup and calibration. Thresholds may be tuned
  on them.
- **Test days (4 and later).**
  - Before any test day is scored, the server version, configuration, thresholds
    and the scoring code are frozen. The commit is recorded.
  - Test days are scored once.
  - A configuration change after that starts a new test period: at least 4 new
    days, recorded after the change.
- **Excluded days.** A day with more than 6 hours excluded (visitors, outages)
  is dropped from its split. It is not replaced by moving another day across.

## Checks before each step

- **Daily:** run `labels.py check` on the label file. It pairs every start with
  an end, flags overlapping states and totals each day:

  ```bash
  python tools/real_home/labels.py check --labels /private/ruview-home/labels.csv \
    --timezone America/Los_Angeles --in-area S1,S2
  ```
- **Before scoring test days:**
  - `labels.py check` shows the minimum test data
  - every hourly capture file's SHA-256 matches the manifest
  - the freeze commit is recorded
- **Results:** they go in `docs/validation/` with the date, tagged MEASURED,
  with the scoring command and the aggregate numbers only.

## Sign-off

| Role | Name | Date |
|---|---|---|
| Study owner (home and hardware) | Justin Wold | Pending: signs when the boards and mmWave module arrive |
| Reviewer | TBD | |

Names recorded 2026-11-21. The protocol stays PROPOSED until the study
owner signs with a date. Still open at that point: the reviewer, and whether
empty-home intervals that the mmWave reference contradicts still count.

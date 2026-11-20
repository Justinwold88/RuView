# Fall detection study protocol (plan item 2.6)

**Status: PROPOSED. It needs the study owner's sign-off before the first
session.** The sections marked *frozen* do not change once a session has been
recorded. A change means a new protocol version and new sessions.

This study scores the **existing** firmware fall detector for detection rate
and false alarms, MEASURED. The detector flags a fall when the phase
acceleration exceeds `fall_thresh` (default 15 rad/s²) for 3 frames in a row,
then stays quiet for 5 s. The study does not tune that threshold. Fall outputs
should stay hidden from Home Assistant by default until this study supports
showing them.

**What it can and cannot show.** Staged falls by healthy adults onto a mat are
not real falls by frail or older people. A good result here means the
detector reacts to deliberate falls in this home. It does not mean RuView
detects real falls, and nothing may be described as a medical or safety
device.

## Safety (frozen)

- **Who.**
  - Only adults who agree in writing, feel fit to do it, and can stop at any
    time.
  - Anyone with a current injury, a bone or joint condition, a balance or
    heart problem, or who is pregnant, does the fall-like actions and normal
    activity only, never the falls.
- **Where.**
  - Every fall lands on a gymnastics landing mat at least 20 cm thick and at
    least 2 m × 1.2 m.
  - Furniture and hard edges are at least 1 m from the mat.
  - Nobody falls from a height, from furniture, or onto a bed frame.
- **How.**
  - Falls are controlled: knees first, then hip and forearm, chin tucked. This
    is practised slowly first.
  - A second adult is present as a spotter and runs the session script.
  - Each person does at most 12 falls per session, with at least 1 minute
    between falls, and does a warm-up first.
  - The session stops at the first sign of pain or dizziness.
- **Nothing is recorded but RF.** The same privacy rules as the
  [real-home dataset](real-home-dataset-protocol.md) apply: no cameras, no
  names, and the data stays private and is never committed.

## Known limit to fix first: the fall flag is not latched

The firmware clears its fall flag on the first frame below the threshold.
Vitals packets go out only every 100–2000 ms (the adaptive controller sets
the interval). A detection can therefore be cleared before any packet carries
it, and the server and Home Assistant never see it.

The study scores two outputs:

1. **Packet detections:** a rising edge of the fall bit (flags bit 1) in any
   vitals (`0xC5110002`) or fused vitals (`0xC5110004`) packet from a node in
   the room, read from the tee capture. This is what users would see.
2. **Log detections:** "Fall detected!" lines in the node's USB serial log,
   when one is captured with host timestamps. This is what the detector
   decided.

If packet detections are fewer than log detections, the packet path is losing
falls. A firmware change that holds the flag until one packet has carried it
should then land, and be validated on hardware, before the result is used.

## Setup

- **Recording.** Use the same nodes and recorders as the real-home dataset:
  `record_tee.py` on the server's `--udp-tee`. Each node's serial log is
  captured over USB if possible.
- **Record once per session:**
  - firmware version and app SHA-256
  - `fall_thresh` and the vitals interval
  - node positions, and the mat position relative to the nodes

## Session (frozen)

`tools/real_home/fall_session.py` runs a session:

- It shuffles the trials with a recorded seed.
- It names each trial to the spotter.
- It records the cue time and the spotter's key press at the moment of impact
  or contact.

| Block | Trials | Kinds |
|---|---|---|
| Falls onto the mat | 3 of each kind, 12 in all | `fall_forward`, `fall_backward`, `fall_sideways`, `fall_from_chair` (slide off a chair onto the mat) |
| Fall-like actions | 3 of each kind, 15 in all | `sit_fast` (drop onto a sofa), `lie_down_fast` (onto a bed), `drop_object` (a 2 kg bag onto the floor), `jump`, `pick_up_fast` |
| Normal activity | 2 blocks of 10 minutes | `walk_around`, `chores` (tidy, carry things, open doors) |

**After each trial,** the person lies or sits still for 10 s and then resets.
Trials start at least 20 s apart, longer than the detector's 5 s quiet time.

**After a trial goes wrong,** the spotter marks it aborted (for example, the
person stopped halfway). Aborted trials are reported and not scored.

**Minimum for a result:**

- at least 30 completed falls
- at least 30 fall-like trials
- at least 60 minutes of normal activity

These come from one or more sessions and at least 2 people, if 2 can take
part safely.

## Scoring (frozen)

- **Detected fall.** A fall counts as detected when a rising edge falls within
  1 s before to 5 s after the impact press. The same rule is applied
  separately to packet detections and log detections.
- **Detection rate.** Detected falls divided by completed falls, with a Wilson
  95% interval. It is also reported per fall kind.
- **False alarms.** Rising edges outside every fall window are false alarms:
  - **during fall-like trials:** the share of trials that raised one (from the
    cue to 10 s after the press), per kind
  - **during normal activity:** false alarms per hour
  - **in daily life:** false alarms per day over the real-home dataset's
    recording, from the same tee capture
- **No tuning.** The threshold is fixed. If these results lead to a new
  threshold, it is tested on new sessions, never on these.

The results go in `docs/validation/` with the date, tagged MEASURED, with
aggregate numbers only.

## Sign-off

| Role | Name | Date |
|---|---|---|
| Study owner | | |
| Spotter | | |

# ADR-430: CSI-BFI-HAR protocol 5 scores the held-out groups once, with a session-normalized feature (amends ADR-365)

- **Status**: proposed (fork; accepted for the fork's public BFI test by the study owner, 2026-11-22)
- **Date**: 2026-11-22
- **Deciders**: RuView maintainers; fork study owner
- **Tags**: bfi, datasets, benchmarks, held-out

## Context

[ADR-365](ADR-365-public-bfi-dataset-and-decoder-contract.md) partitions the
pinned CSI-BFI-HAR BFI captures into four groups:

| Group | Source | Day and room |
|---|---|---|
| train | HAR-1/BFI/M1 | day 1, kitchen |
| validation | HAR-3/BFI/M1 | day 3, classroom |
| heldout_same_device | HAR-5/BFI/M1 | day 5, living room |
| heldout_other_device_view | HAR-5/BFI/M2 | day 5, living room, same events |

It says to "inspect held-out quality without model selection against held-out
outcomes". Protocols 1 to 4 went further and never scored the held-out groups.

**MEASURED, protocol 4 run**
([results](../../tools/bfi_learning/PUBLIC-BFI-MOTION-4.md#results), run
44975936986, under [ADR-428](ADR-428-csi-bfi-har-sequence-control-counter-policy.md)
and [ADR-429](ADR-429-csi-bfi-har-protocol-4-without-activity-s.md)):

- The benchmark was admitted for the 19 activities other than S.
- The one-feature BFI motion detector, fitted on train, scored balanced
  accuracy **0.500** on validation. Its threshold (0.0327 rad) sat below every
  validation activity's median, so it called every interval moving.
- The feature's level moved between groups: train medians 0.018–0.103 rad,
  validation medians 0.093–0.148 rad. Within validation, moving (0.097–0.148)
  and still (0.093–0.144) medians still overlapped.

So the frozen raw feature does not transfer across a change of room and day.
One open question is whether a label-free, per-session rescaling removes the
level shift. The study owner asked to continue onto the held-out groups, which
no protocol has scored.

## Decision

Define protocol version **`public-bfi-motion-5`**. It is
[version 4](../../tools/bfi_learning/PUBLIC-BFI-MOTION-4.md) with three
changes. Everything else, including the ADR-428 packet policy and ADR-429's
exclusion of activity S, applies unchanged.

1. **Session normalization, with no labels.** Each eligible interval's motion
   feature is divided by the median motion feature over every eligible
   interval in the same group, every activity included (S too). Reports per
   interval are scaled the same way for the timing-only control. Each group is
   one session: one day, one room, one device view.
   - Per-recording normalization was rejected. Each recording is one activity,
     so dividing by its own median would erase the label.
   - This assumes every session covers a similar mix of activities. The
     dataset's design gives each group the same activities and participants.
2. **The threshold is fitted on train + validation together**, on the
   normalized feature, with the version 1 threshold rule (highest balanced
   accuracy; ties go to the lower threshold, then "greater").
   - A **design check** is also reported: the same rule fitted on normalized
     train only and scored on normalized validation. It runs in the same job as
     the held-out score, so it cannot inform any choice. It is a diagnostic.
3. **The held-out groups are scored once.**
   - **Primary result:** interval balanced accuracy of the BFI motion feature
     on `heldout_same_device` (M1, the device the threshold was fitted on),
     moving as the positive class.
   - **Its 95% interval:** recordings are resampled with replacement within
     each class (2,000 resamples, NumPy seed 20261122), because intervals from
     one recording are not independent.
   - **Decision rule:** the feature is reported as **better than chance on
     held-out data** only if that interval's lower bound is above 0.5.
   - **Also reported, never a gate:** recording-level balanced accuracy; the
     same numbers for `heldout_other_device_view` (M2); the timing-only
     control; the constant-class control; the session scales; per-activity
     normalized medians for all four groups.

**Freezing and the one run.**

- The idea for this design came from protocol 4's train and validation
  numbers only. No held-out feature value, label or score existed for any
  version when it was chosen.
- The protocol document, this ADR and the code are frozen before the first
  dispatch. The first run that reaches the scoring step is the result.
- A run that fails before scoring (download, runner or tool failure) may be
  dispatched again at the same commit and protocol, with no change.
- If admission fails, nothing is scored and the held-out groups stay unscored.
- Any later change, or any later score on these groups, is a new protocol, and
  its held-out numbers are no longer held-out results. They must say so.

**What does not change:**

- Versions 1 to 4 stay reproducible and their results stand.
- ADR-365's rules govern every other protocol and any future benchmark on this
  manifest.
- The live capture path is untouched.

## Consequences

### Positive

- The first score on data from a room and day that played no part in any
  choice. This is the only test in the study that can show transfer.
- If the result is null, it is a clean null: no tuning was possible against
  it.

### Negative

- The held-out groups are used up for this feature family. Any later study on
  them is exploratory.
- The two held-out views record the same events, so they are not two
  independent results. The same-device view is primary.
- The confidence interval treats recordings as independent. Recordings by the
  same participant are not, and there are only three participants, so it may
  be too narrow.
- The normalization would mislead in a session with a very different mix of
  activities, such as a real home where someone sits still for most of the
  day.

### Neutral

- Offline research tooling only. It is not a production-integration decision,
  and no accuracy claim beyond this public dataset follows from it.
- Results stay labelled with the protocol name and are not comparable to
  versions 1 to 4, or to any A–T result.

## Validation

- `tools/bfi_learning/test_public_bfi_motion.py` (`Protocol5Tests`), on
  SYNTHETIC records:
  - only protocol 5 scores held-out, and it keeps the ADR-428 policy and
    leaves S out
  - normalization ignores labels, uses the plain group median, and refuses a
    zero scale
  - a per-session level shift that defeats the protocol 4 scorer is removed
  - flipping the held-out labels changes neither the fit nor the design check
  - the final threshold comes from train + validation, not train alone
  - S is neither fitted nor scored, and random features are not reported as
    better than chance
  - the bootstrap is deterministic and bounded
  - the report states the primary result, the decision and the limitations
- The [protocol v5](../../tools/bfi_learning/PUBLIC-BFI-MOTION-5.md) run in
  `public-bfi-motion.yml` reports the census, admission and, if admitted, the
  held-out scores.

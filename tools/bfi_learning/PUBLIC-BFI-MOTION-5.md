# Public BFI motion test, protocol version 5 (`public-bfi-motion-5`)

Status: **frozen before its first run.** Results go in the section at the end,
added after the run. Nothing above that section may change once the run has
been dispatched. The study owner approved
[ADR-430](../../docs/adr/ADR-430-csi-bfi-har-protocol-5-session-normalized-heldout-score.md)
for this run on 2026-11-22.

Version 5 is [version 4](PUBLIC-BFI-MOTION-4.md) with three changes:
- **The feature is normalized per session**, with no labels.
- **The threshold is fitted on train + validation.**
- **The held-out groups are scored, once.**

Everything else in version 4 applies unchanged:
- the data and the four groups
- no CSI
- file admission
- packet admission, including the named policy
  `csi-bfi-har-seqctl-counter-v1` from
  [ADR-428](../../docs/adr/ADR-428-csi-bfi-har-sequence-control-counter-policy.md)
- the decoder oracle
- the observed-interval rules and the raw features
- benchmark admission for the 19 activities other than S
  ([ADR-429](../../docs/adr/ADR-429-csi-bfi-har-protocol-4-without-activity-s.md))
- the moving labels and the threshold rule

Versions 1 to 4 keep their results. Version 5 results are labelled with the
protocol name and are not comparable to any earlier version or to any A–T
result.

## Why a fifth version

Version 4 was admitted and its detector scored balanced accuracy 0.500 on
validation. The feature's level moved between train (kitchen, day 1) and
validation (classroom, day 3): every validation activity, still ones included,
sat above the train threshold.

Version 5 asks one question: **once each session is rescaled by its own
median, without labels, does the one-feature BFI motion statistic tell moving
from still activity in a room and day that played no part in any choice?**

The design came from version 4's train and validation numbers only. No
held-out feature value, label or score existed for any version when it was
chosen.

## What changes

**Session normalization.** A session is one group: one day, one room, one
device view.
- For each group, take the median motion feature over every eligible interval
  in that group, every activity included (S too). That is the group's motion
  scale.
- Divide each interval's motion feature by its group's scale.
- Do the same with reports per interval for the timing-only control.
- No label enters the scale. Per-recording normalization is not used: each
  recording is one activity, so it would erase the label.
- A scale that is not positive stops the run with
  `session_scale_not_positive:<group>`.

**Fitting.** The threshold is fitted on the normalized train **and**
validation intervals together, 19 activities, with the version 1 rule:
highest balanced accuracy; ties go to the lower threshold, then "greater".

**Design check (diagnostic only).** The same rule is fitted on normalized
train alone and scored on normalized validation. It runs in the same job as
the held-out score, so it cannot inform any choice.

**Held-out scoring.** The final threshold is applied once to each held-out
group:

| Result | Group | Measure |
|---|---|---|
| **Primary** | `heldout_same_device` (M1) | Interval balanced accuracy of the BFI motion feature, moving as the positive class |
| Secondary | both held-out groups | Interval and recording balanced accuracy (recording value = median of its intervals) for the BFI motion feature, the timing-only control and the constant class |

**The 95% interval and the decision rule.**
- Recordings are resampled with replacement within each class, 2,000 times,
  NumPy `default_rng(20261122)`. Each resample's balanced accuracy is computed
  from the pooled intervals of the chosen recordings. The interval is the 2.5th
  to 97.5th percentile.
- The report says **better than chance: YES** only if the primary result's
  lower bound is above 0.5. Otherwise it says **NO**.
- The timing-only control and the M2 view are reported next to it and never
  change the decision.

**Labels.** As in version 4:

| Group | Codes |
|---|---|
| **Moving** | A jogging, B clapping, C push forward, D boxing, G rotating, K waving, L walking, O hands up-down, Q side bends |
| **Still / fine-motor** | E writing, F brushing teeth, H standing, I eating, J reading a book, M browsing phone, N drinking, P phone call, R check wrist, T browsing laptop |
| **Left out** | S washing hands |

**Reporting.** The report holds, as before, aggregate numbers only, plus:
- the session scales for all four groups
- the final threshold and its train + validation balanced accuracy
- the design check
- both held-out groups' scores, with the 95% intervals
- per-activity normalized medians and interval counts for all four groups

## The one run

- The first run that reaches the scoring step is the result.
- A run that fails before scoring (download, runner or tool failure) may be
  dispatched again at the same commit and protocol, with no change.
- If admission fails, nothing is scored and the held-out groups stay unscored.
- Any later change, or any later score on these groups, is a new protocol.
  Its held-out numbers are no longer held-out results, and it must say so.

## Known limits

- The two held-out views record the same events with different devices. They
  are not two independent results.
- The 95% interval treats recordings as independent. Recordings by the same
  participant are not, and there are only three participants, so the interval
  may be too narrow.
- The normalization assumes every session covers a similar mix of activities,
  as this dataset's design does. It would mislead in a session with a very
  different mix, such as a real home where someone sits still most of the day.
- Every recording has a person present, so presence is not tested. Day and
  room are confounded in every group.

## Reproducer

The workflow is `.github/workflows/public-bfi-motion.yml`, run with
`protocol: public-bfi-motion-5`. To run it locally:

```bash
python tools/bfi_learning/download_public_bfi.py --include-heldout --output /private/bfi
python tools/bfi_learning/public_bfi_motion.py --protocol public-bfi-motion-5 \
  --data /private/bfi --report report.md --json report.json
```

## Results

Not yet run.

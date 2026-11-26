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

**Run 1 (2026-11-26 UTC): benchmark ADMITTED. On held-out data the BFI
motion feature is NOT better than chance.** MEASURED on public data, under the
policy `csi-bfi-har-seqctl-counter-v1` with activity S left out (ADR-429) and
the ADR-430 session normalization. Not comparable to versions 1–4 or to any
A–T result.
- Workflow run [45732394943](https://github.com/Justinwold88/RuView/actions/runs/45732394943), commit `f7c705f3`.
- Dataset revision `c3013b3e…`, manifest SHA-256 `c2db11f7…`, Wireshark 4.2.2.

**Census, oracle and admission: the same as version 4.**
- All 240 files were admitted, with no repeated 16-bit Sequence Control value
  within 1 s.
- The decoder oracle matched 1,872 reports with tshark 4.2.2; 960 of them were
  admitted under the policy.
- Recordings with an eligible interval: train 57 of 60, validation 41 of 60,
  held-out 60 of 60 in each view.
- All 19 activities have at least 2 recordings with an eligible interval in
  all four groups.

**Primary result: BFI motion on `heldout_same_device`, interval balanced
accuracy 0.479, 95% interval 0.376–0.578. Better than chance: NO.**

Thresholds were fitted on normalized train + validation and scored once on
each held-out view, with moving as the positive class. Cells show balanced
accuracy (TP/FN/TN/FP):

| Detector | Threshold (normalized) | Fit BA | Same device, intervals | 95% interval | Same device, recordings | Other view, intervals | 95% interval | Other view, recordings |
|---|---|---|---|---|---|---|---|---|
| BFI motion feature | greater than 1.020 × session median | 0.594 | **0.479** (688/942/959/831) | **0.376–0.578** | 0.470 (11/16/16/14) | 0.584 (984/665/1040/782) | 0.474–0.691 | 0.596 (16/11/18/12) |
| Timing-only control | fewer than 0.708 × session median reports | 0.526 | 0.520 (237/1393/1603/187) | 0.474–0.568 | 0.519 (1/26/30/0) | 0.508 (177/1472/1654/168) | 0.470–0.546 | 0.519 (1/26/30/0) |
| Constant class (still) | | | 0.500 (0/1630/1790/0) | | | 0.500 (0/1649/1822/0) | | |

- The same-device view scored 3,420 intervals (1,630 moving, 1,790 still)
  from 57 recordings. The other view scored 3,471 intervals from the same 57
  events.
- The other view's 0.584 is a secondary result, and its interval also
  includes 0.5. It records the same events, so it is not a second test.
- Design check, which played no part in any choice (fitted on normalized
  train, scored on normalized validation): BFI motion 0.522, timing-only
  0.496. Without normalization, version 4 scored 0.500 there.

**Session scales** (median raw motion feature, median reports per interval):

| Group | Motion scale (rad) | Reports scale |
|---|---|---|
| train (kitchen, day 1) | 0.0288 | 118 |
| validation (classroom, day 3) | 0.1012 | 159 |
| held-out, same device (living room, day 5) | 0.1080 | 258 |
| held-out, other view (same events) | 0.0376 | 265 |

**What happened.**
- **Normalization removed the level shift, and little signal was left.** In
  validation and the same-device held-out view, every activity's normalized
  median except two sits between 0.92 and 1.13 times its session median,
  moving and still alike.
- **The two exceptions are jogging (A, moving) and reading a book (J, still).**
  J's median is in the top two of all 19 activities in every group, and the
  highest in both held-out views (1.635 and 2.921). Whatever the feature
  responds to, reading shares it with jogging. This dataset can't say why.
- **The device view moves the level as much as room and day do.** The two
  held-out views record the same events, yet their motion scales differ by a
  factor of about 2.9.
- **Even the fit was weak.** Balanced accuracy on train + validation was only
  0.594, before any held-out data was touched.

**Answer to the question.** On this public data, the one-feature BFI motion
statistic does not tell sustained large motion from fine-motor or still
activity in a room and day that played no part in any choice, even after each
session is rescaled by its own median. Its same-device held-out score, 0.479,
is no better than always predicting one class, and the timing-only control
does no better.

This closes plan item 1.6 on this dataset with a null result.
- The held-out groups have now been scored. Any later study on them is
  exploratory, and its numbers are not held-out results (ADR-430).
- Regrouping activities (for example, moving J to "moving") would be chosen
  after seeing held-out numbers, so it can't be tested here.
- A different feature or model would need new held-out data, such as the
  consented real-home recordings in Phase 2.
- Nothing in RuView's live path depends on this result.

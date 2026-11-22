# Public BFI motion test, protocol version 4 (`public-bfi-motion-4`)

Status: **frozen before its first run.** Results go in the section at the end,
added after the run. Nothing above that section may change once the run has
been dispatched. The study owner approved
[ADR-429](../../docs/adr/ADR-429-csi-bfi-har-protocol-4-without-activity-s.md)
for this run on 2026-11-21.

Version 4 is [version 3](PUBLIC-BFI-MOTION-3.md) with one change: **activity S
(washing hands) is left out of benchmark admission and scoring.** ADR-429
amends ADR-365's A–T coverage rule for this protocol only.

Everything else in version 3 applies unchanged:
- the data and the four groups
- held-out groups inspected for quality only, never scored
- no CSI
- file admission
- packet admission, including the named policy
  `csi-bfi-har-seqctl-counter-v1` from
  [ADR-428](../../docs/adr/ADR-428-csi-bfi-har-sequence-control-counter-policy.md)
- the decoder oracle
- the observed-interval rules
- the moving labels, detector, controls, threshold rule and reporting

Versions 1, 2 and 3 keep their results. Version 4 results are labelled with
the protocol name and the excluded code, and are not comparable to any
earlier version or to any A–T result.

## Why a fourth version

Version 3 admitted every file and every candidate report. Coverage then fell
short in exactly one of the 80 group/activity cells: train activity S has no
recording with an eligible interval. Under ADR-365, that one cell keeps the
benchmark from ever being admitted on this manifest.

The choice to leave S out was made from version 3's coverage table only. No
score, feature value or per-activity statistic existed for any version when it
was made: no version has been admitted, so none was scored.

## What changes

**The census.** No change. Every manifest file goes through every census step
and the decoder oracle, S included. Coverage is reported for all 20 codes,
with S marked as left out.

**Benchmark admission.** For each of the four groups and each activity **A–T
except S**, at least two different recordings must supply at least one
eligible interval. The benchmark is not admitted, and no detector is scored,
if either of these happens:
- any of those 19 activities in any group falls short
- the decoder oracle fails, in any file, S files included

**Labels.**

| Group | Codes |
|---|---|
| **Moving** (unchanged) | A jogging, B clapping, C push forward, D boxing, G rotating, K waving, L walking, O hands up-down, Q side bends |
| **Still / fine-motor** | E writing, F brushing teeth, H standing, I eating, J reading a book, M browsing phone, N drinking, P phone call, R check wrist, T browsing laptop |
| **Left out** | S washing hands |

**Fitting and scoring.** S intervals are neither used to fit the threshold
nor scored. The constant-class control counts train intervals of the 19 codes
only. The per-activity table lists the 19 codes.

**Nothing else changes.** No other activity, group, participant, interval
rule or threshold rule changes. Validation participant P3 has no eligible
interval for 19 of 20 activities. That is reported as measured, not
corrected.

## Reproducer

The workflow is `.github/workflows/public-bfi-motion.yml`, run with
`protocol: public-bfi-motion-4`. To run it locally:

```bash
python tools/bfi_learning/download_public_bfi.py --include-heldout --output /private/bfi
python tools/bfi_learning/public_bfi_motion.py --protocol public-bfi-motion-4 \
  --data /private/bfi --report report.md --json report.json
```

## Results

**Run 1 (2026-11-22): benchmark ADMITTED. The BFI motion detector scored at
chance on validation.** MEASURED on public data, under the policy
`csi-bfi-har-seqctl-counter-v1` with activity S left out (ADR-429). Not
comparable to versions 1–3 or to any A–T result.
- Workflow run [44975936986](https://github.com/Justinwold88/RuView/actions/runs/44975936986), commit `d94b4b0c`.
- Dataset revision `c3013b3e…`, manifest SHA-256 `c2db11f7…`, Wireshark 4.2.2.

**Census and oracle: the same as version 3.**
- All 240 files were admitted, and no candidate repeated a 16-bit Sequence
  Control value within 1 s.
- The decoder oracle matched 1,872 reports from all 240 files with
  tshark 4.2.2; 960 of them were admitted under the policy.
- Recordings with an eligible interval: train 57 of 60, validation 41 of 60,
  held-out 60 of 60 each.

**Admission.** Every one of the 19 activities has at least 2 recordings with
an eligible interval in all four groups. Train has 3 for every activity and
validation 2. S stays as measured: 0 recordings in train and 3 in validation.

**Scores**, fitted on train intervals and scored on validation, with moving
as the positive class:

| Detector | Threshold | Train BA | Validation interval BA (TP/FN/TN/FP) | Validation recording BA (TP/FN/TN/FP) |
|---|---|---|---|---|
| BFI motion feature | greater than 0.0327 rad | 0.643 | **0.500** (1098/0/0/1210) | **0.500** (18/0/0/20) |
| Timing-only control | fewer than 98.5 reports | 0.531 | 0.521 (165/933/1079/131) | 0.503 (1/17/19/1) |
| Constant class (still) | | | 0.500 (0/1098/1210/0) | |

Validation scored 2,308 intervals from 38 recordings: 1,098 moving and 1,210
still. The 149 S intervals in validation were left out, as frozen.

**What happened.** The threshold fitted on train labelled every validation
interval as moving. The feature's level moved between the groups:
- Train medians run from 0.018 to 0.103 rad across the 19 activities.
- Validation medians run from 0.093 to 0.148 rad. Every activity, still ones
  included, sits above the train threshold.
- Train and validation differ in room (kitchen vs classroom) and day (1 vs 3),
  and the protocol can't separate the two.
- Within each group the activities overlap too. In validation, the moving
  medians are 0.097–0.148 rad and the still medians 0.093–0.144 rad. Reading
  a book (J, still) has the second-highest median in both groups.
- Train balanced accuracy was only 0.643, so even the fit on train was weak.

**Answer to the question.** On this public data, the frozen one-feature BFI
motion statistic does not tell sustained large motion from fine-motor or
still activity across a change of room and day. It does no better than always
predicting one class, and the timing-only control does barely better
(0.521). A
better result would need features that are normalised per recording or
session, or training data from the room being scored. Either would be a new,
separately frozen study. Nothing here was tuned on validation, and the
held-out groups were inspected for quality only, never scored.

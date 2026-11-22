# ADR-429: CSI-BFI-HAR protocol 4 leaves activity S out of the motion benchmark (amends ADR-365)

- **Status**: proposed (fork; accepted for the fork's public BFI test by the study owner, 2026-11-21)
- **Date**: 2026-11-21
- **Deciders**: RuView maintainers; fork study owner
- **Tags**: bfi, datasets, benchmarks, coverage

## Context

[ADR-365](ADR-365-public-bfi-dataset-and-decoder-contract.md) requires, for
benchmark admission, at least two independent recordings per activity in each
partition/view group that supply an eligible interval: "Missing A–T coverage
fails benchmark admission." Its Alternatives section warns that "silently
dropping sparse classes would change the task while hiding the change."

**MEASURED, protocol 3 run**
([results](../../tools/bfi_learning/PUBLIC-BFI-MOTION-3.md#results), run
44897677546, under [ADR-428](ADR-428-csi-bfi-har-sequence-control-counter-policy.md)):

- All 240 files passed integrity, inventory and lossless conversion. The
  decoder oracle matched 1,872 reports with tshark 4.2.2.
- 79 of the 80 group/activity cells have two or more recordings with an
  eligible interval.
- **The short cell is train activity S** (washing hands; all three
  participants, day 1, kitchen). None of its recordings has an eligible
  interval.

So under ADR-365 the benchmark can never be admitted on this manifest while S
is part of the task, whatever happens to the other 19 activities. No detector
has been scored in protocols 1, 2 or 3, because none was admitted.

## Decision

Define protocol version **`public-bfi-motion-4`**. It is
[version 3](../../tools/bfi_learning/PUBLIC-BFI-MOTION-3.md) with one change:
**activity S is left out of benchmark admission and scoring.** The task becomes
moving versus still over 19 activity codes, A–T without S.

This amends ADR-365's A–T coverage rule for protocol 4 only. The exclusion is
explicit, named and reported, not silent:

1. **The census is unchanged.** Every manifest file, S included, still goes
   through integrity, inventory, lossless conversion, packet admission under
   `csi-bfi-har-seqctl-counter-v1` and the decoder oracle. Any oracle mismatch
   in an S file still fails admission.
2. **Coverage is still reported for all 20 codes.** The report marks S as left
   out, so its coverage stays visible.
3. **Admission needs two recordings with an eligible interval for each of the
   other 19 codes, in every one of the four groups.**
4. **S intervals are neither fitted nor scored.** The threshold is fitted on
   train intervals of the 19 codes and scored on validation intervals of the
   same 19 codes. S is a still/fine-motor code, so the still group loses one of
   its eleven codes and the moving group is unchanged.
5. **Only S.** The choice was made from version 3's coverage table alone,
   before any score, feature value or per-activity statistic existed for any
   version. No other activity, group, participant or rule changes. In
   particular, validation participant P3 has no eligible interval for 19 of 20
   activities, and that stays as measured.
6. **Labelled and not comparable.** Results carry the protocol name and the
   excluded code, and are not comparable to versions 1, 2 or 3, or to any
   A–T result.
7. **Held-out groups** are still inspected for quality only and never scored.

**What does not change:**

- Versions 1, 2 and 3 stay reproducible, and their results stand.
- ADR-365's A–T rule still governs every other protocol and any future
  benchmark on this manifest.
- The live capture path is untouched.

## Consequences

### Positive

- The benchmark can be admitted on the coverage that exists. The question
  ("can a simple BFI statistic tell sustained large motion from fine-motor or
  still activity?") is answered on 19 of the 20 activities, with the gap
  stated.

### Negative

- The task is narrower than ADR-365's. A detector that confuses washing hands
  with motion would not be caught here.
- This is the fourth look at the same files' census. Each change so far was
  made from census and coverage numbers only, never from labels against
  features or scores, but the protocol history should be read as a whole.

### Neutral

- Offline research tooling only. It is not a production-integration decision,
  and no accuracy claim beyond this public dataset follows from it.

## Validation

- `tools/bfi_learning/test_public_bfi_motion.py` (`Protocol4Tests`), on
  SYNTHETIC records:
  - only protocol 4 leaves S out, and S is a still code
  - the census still counts S files and S coverage
  - a train S shortfall blocks version 3 and not version 4
  - every other activity and the oracle are still required
  - S intervals are neither fitted nor scored
  - the report marks S and cites this ADR
- The [protocol v4](../../tools/bfi_learning/PUBLIC-BFI-MOTION-4.md) run in
  `public-bfi-motion.yml` reports the census for all 20 codes next to the
  19-code admission and, if admitted, the scores.

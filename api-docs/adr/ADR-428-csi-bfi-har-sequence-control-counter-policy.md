# ADR-428: CSI-BFI-HAR Sequence Control counter policy (amends ADR-365 and ADR-427)

- **Status**: proposed (fork; accepted for the fork's public BFI test by the study owner, 2026-11-21)
- **Date**: 2026-11-20 (accepted for the fork 2026-11-21)
- **Deciders**: RuView maintainers; fork study owner
- **Tags**: bfi, datasets, decoding, fragmentation, benchmarks

## Context

[ADR-365](ADR-365-public-bfi-dataset-and-decoder-contract.md) quarantines
public BFI frames whose Sequence Control fragment number is nonzero until a
reviewed dataset policy justifies admitting them.
[ADR-427](ADR-427-csi-bfi-har-complete-mpdu-fragment-policy.md) defined the
first such policy, `csi-bfi-har-complete-mpdu-v1`. It admits a complete,
CRC-valid Action No Ack report with a nonzero fragment number only when no
other frame from the same pair shares its 12-bit sequence number with a
different fragment number within 1 s.

**MEASURED, protocol 2 run**
([results](../../tools/bfi_learning/PUBLIC-BFI-MOTION-2.md#results), run
44695427917):

- **Integrity and decoding:** all 240 files passed. The decoder oracle
  matched 1,293 reports with tshark 4.2.2, 381 of them policy reports.
- **No candidate failed the policy's form rules.** Every nonzero-fragment
  candidate was Action No Ack, with a verified FCS and a complete body.
- **The sibling rule refused almost all of them.** It admitted 1.2% of the
  nonzero-fragment candidates in train, 0.3% in validation and under 0.01% in
  the held-out groups.
- **The 16 fragment-number values are equally common:** 6.2–6.3% each, in
  every group.
- **The benchmark was still not admitted.** 37 group/activity cells have
  fewer than 2 recordings with an eligible interval.

Under strict mode and under protocol 2, no candidate feedback frame in any
file had More Fragments set: the MAC-guard rejection count was zero in every
group.

These observations fit one reading: the capture writes a 16-bit counter into
the whole Sequence Control field, so the 12-bit "sequence number" changes only
every 16 frames. ADR-427's sibling rule treats those 16 consecutive frames as
fragments of one MSDU and refuses them.

Real 802.11 fragmentation has a signature that these files lack. Every
fragment except the last sets More Fragments, and each fragment carries part
of an MSDU, not a complete report.

## Decision

Define the named policy **`csi-bfi-har-seqctl-counter-v1`**. It applies only
to files from the pinned CSI-BFI-HAR manifest (revision `c3013b3e…`),
processed by `tools/bfi_learning/public_bfi_motion.py` with
`--protocol public-bfi-motion-3`. It replaces ADR-427's policy in that
protocol only.

A frame with a nonzero fragment number is admitted only if **all** of these
hold. Otherwise it stays quarantined, and the reason is counted:

1. **The usual checks.** Every rule that ADR-427 rule 1 lists still applies:
   - integrity, inventory and lossless conversion
   - radiotap, and the management Action category 21/0
   - ToDS, FromDS, More Fragments, Protected and Order all clear
   - the file's direction
   - `public_bfi_decode.decode_body`: exactly 1,003 bytes, VHT MU 3x1,
     80 MHz, Ng=1, codebook 1, `first_segment = 1`, `remaining_segments = 0`
   - the retry and time-order rules
2. **Subtype.** It is Action No Ack (subtype 14). Otherwise
   `quarantined_policy_not_action_no_ack`.
3. **FCS.** Radiotap declares an FCS and it matches a CRC-32 over the
   unmodified MAC bytes. Otherwise `quarantined_policy_no_fcs`.
4. **No fragmentation in the file.** No feedback frame in the file (Action
   category 21/0, any pair) has More Fragments set. If one does, every
   nonzero-fragment candidate in that file is refused:
   `quarantined_file_has_more_fragments`. A fragment train can't exist
   without More Fragments frames, and ADR-427's sibling rule is dropped
   because of this file-level check.
5. **No repeated counter value.** No other candidate from the same
   transmitter and receiver has the same full 16-bit Sequence Control value
   within 1 s. Otherwise `quarantined_seqctl_duplicate`. A counter does not
   repeat within a second. A repeat would point to retransmission or to a
   field that is not a counter.
6. **Nothing rewritten.** No header, Sequence Control or FCS byte is changed,
   and the raw fragment-number distribution is reported per group.
7. **Oracle.** As in ADR-427: in every file, the first four reports admitted
   under this policy are compared with tshark 4.2.2, in addition to the first
   four fragment-zero reports. Any mismatch fails benchmark admission.

A frame with fragment number 0 keeps its strict-mode handling.

**Diagnostic, never a gate.** Each step of the 16-bit Sequence Control value
between consecutive candidates of the chosen direction is reported per group:
`counter_step_1`, `counter_step_2_to_16`, `counter_step_17_to_4096` and
`counter_step_other` (zero, backward or larger). It shows how far the field
behaves like a counter. It is not used to admit or refuse frames, because a
frame-level counter rule would need step limits that could only be set by
looking at the data first.

**What does not change:**

- **The live path.** `tools/bfi`, `prepare_bfi.py` and the Rust sensing
  server keep rejecting nonzero fragment numbers on Action frames.
- **Earlier protocols.** `public-bfi-motion-1` and `public-bfi-motion-2`
  stay unchanged and reproducible, and their results stand.
- **The policy's reach.** It reinterprets no other dataset or capture.

A new protocol version, `public-bfi-motion-3`, adopts this policy. That
version is frozen before it runs: the data, groups, interval rules, labels,
detector, controls, threshold rule and scoring are those of version 1.
Held-out groups are still inspected for quality only and never scored.

## Consequences

### Positive

- If the counter reading is right, nearly every complete report in the train
  and validation groups is admitted. The benchmark can then be judged on its
  coverage, not on a capture artifact.
- Every report admitted this way is still complete, CRC-valid,
  oracle-checked and counted separately.

### Negative

- If a file's frames are real fragments whose More Fragments frames were all
  lost from the capture, rule 4 can't see that. The exact-length,
  single-segment, FCS and oracle checks still keep each admitted report
  complete. Only its timing meaning would be in doubt.
- Protocol 3 is the third look at the same files' quality census. The census
  and the policy design used no labels, angles or detector output, and no
  detector has been scored. The admission and scoring rules are unchanged
  from version 1.
- Results are labelled with the policy name and are not comparable to
  versions 1 or 2.

### Neutral

- This is a dataset adapter for offline research. It is not a
  production-integration decision.

## Validation

- `tools/bfi_learning/test_public_bfi_motion.py` (`CounterPolicyTests` and
  two tshark-backed tests) covers the policy on SYNTHETIC frames:
  - a 16-bit counter, including the wrap past 65535, that ADR-427 refuses
    and this policy admits whole
  - a More Fragments frame that refuses the whole file
  - repeated 16-bit values
  - the subtype, FCS and body-length refusals
  - the step diagnostic
  - refusal to combine the two policies
  - census and oracle runs with tshark and editcap 4.2.2
- The [protocol v3](../../tools/bfi_learning/PUBLIC-BFI-MOTION-3.md) run in
  `public-bfi-motion.yml` reports the policy counts and the step diagnostic
  next to the census.

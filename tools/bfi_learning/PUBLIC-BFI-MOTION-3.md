# Public BFI motion test, protocol version 3 (`public-bfi-motion-3`)

Status: **frozen before its first run.** Results go in the section at the end,
added after the run. Nothing above that section may change once the run has
been dispatched. The run waits for the study owner to approve
[ADR-428](../../docs/adr/ADR-428-csi-bfi-har-sequence-control-counter-policy.md).

Version 3 is [version 1](PUBLIC-BFI-MOTION.md) with one change. Packet
admission step 5, the fragment quarantine, is replaced by the named dataset
policy **`csi-bfi-har-seqctl-counter-v1`** from ADR-428. ADR-428 amends
ADR-365 and, for this protocol only, replaces
[version 2](PUBLIC-BFI-MOTION-2.md)'s policy `csi-bfi-har-complete-mpdu-v1`.
Everything else in version 1 applies unchanged:
- the data and the four groups
- held-out groups inspected for quality only, never scored
- no CSI
- file admission
- the observed-interval rules
- benchmark admission
- the labels, detector, controls, threshold rule and scoring
- reporting

Versions 1 and 2 keep their results. Version 3 results are labelled with the
policy name and are not comparable to either.

## Why a third version

Version 2 admitted only 1.2% of the nonzero-fragment candidates in train,
0.3% in validation and under 0.01% held out. Every candidate passed its form
rules, but almost all of them had a "fragment sibling": another frame with
the same 12-bit sequence number within 1 s. The 16 fragment-number values
were equally common, and no feedback frame had More Fragments set. That fits
a 16-bit counter written across the whole Sequence Control field. ADR-428
treats the field that way, and uses the absence of More Fragments frames, not
the sibling rule, to rule out fragmentation.

## Packet admission step 5 in version 3

A frame with fragment number 0 is handled exactly as in version 1. A frame
with a nonzero fragment number is a **policy candidate**. It continues
through steps 6–9 only if all of these hold:

1. **Subtype:** it is Action No Ack (subtype 14).
2. **FCS:** radiotap declares an FCS, so `strip_radiotap` checked the CRC-32
   over the unmodified MAC bytes and removed exactly four bytes.
3. **No fragmentation in the file:** no feedback frame in the file has More
   Fragments set.
4. **No repeated counter value:** no other candidate from the same
   transmitter and receiver has the same 16-bit Sequence Control value within
   1 s.

Otherwise it stays quarantined and is counted under its reason:
- `quarantined_policy_not_action_no_ack`
- `quarantined_policy_no_fcs`
- `quarantined_file_has_more_fragments` (every policy candidate in that file)
- `quarantined_seqctl_duplicate`

The direction pair in step 6 is chosen from every candidate that passed steps
1–5, so policy candidates count too. A policy candidate that is finally
admitted is counted as `admitted_under_fragment_policy`.

Two diagnostics are reported per group. Neither admits or refuses anything:
- the raw fragment numbers of the step-4 candidates, 0 to 15
- the step of the 16-bit Sequence Control value between consecutive
  candidates of the chosen direction: `counter_step_1`,
  `counter_step_2_to_16`, `counter_step_17_to_4096` and `counter_step_other`

## Decoder oracle in version 3

For every admitted file, tshark 4.2.2 checks two samples:
- the first four admitted fragment-zero reports
- the first four reports admitted under the policy

The match rules are unchanged from version 1. Any mismatch means the
benchmark is not admitted.

## Reproducer

The workflow is `.github/workflows/public-bfi-motion.yml`, run with
`protocol: public-bfi-motion-3`. To run it locally:

```bash
python tools/bfi_learning/download_public_bfi.py --include-heldout --output /private/bfi
python tools/bfi_learning/public_bfi_motion.py --protocol public-bfi-motion-3 \
  --data /private/bfi --report report.md --json report.json
```

## Results

Not yet run.

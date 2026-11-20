# Public BFI motion test, protocol version 2 (`public-bfi-motion-2`)

Status: **frozen before its first run.** Results go in the section at the end,
added after the run. Nothing above that section may change once the run has
been dispatched.

Version 2 is [version 1](PUBLIC-BFI-MOTION.md) with one change. Packet
admission step 5, the fragment quarantine, is replaced by the named dataset
policy **`csi-bfi-har-complete-mpdu-v1`** from
[ADR-427](../../docs/adr/ADR-427-csi-bfi-har-complete-mpdu-fragment-policy.md),
which amends ADR-365. Everything else in version 1 applies unchanged:
- the data and the four groups
- held-out groups inspected for quality only, never scored
- no CSI
- file admission
- the observed-interval rules
- benchmark admission
- the labels, detector, controls, threshold rule and scoring
- reporting

Version 1's result stays as it is. Version 2 results are labelled with the
policy name and are not comparable to version 1.

## Packet admission step 5 in version 2

A frame with fragment number 0 is handled exactly as in version 1. A frame
with a nonzero fragment number is a **policy candidate**. It continues
through steps 6–9 only if all of these hold:

1. **Subtype:** it is Action No Ack (subtype 14).
2. **FCS:** radiotap declares an FCS, so `strip_radiotap` checked the CRC-32
   over the unmodified MAC bytes and removed exactly four bytes.
3. **No fragment siblings:** no other candidate from the same transmitter and
   receiver has the same 12-bit sequence number with a different fragment
   number within 1 s.

Otherwise it stays quarantined and is counted under its reason:
- `quarantined_policy_not_action_no_ack`
- `quarantined_policy_no_fcs`
- `quarantined_fragment_sibling`

The direction pair in step 6 is chosen from every candidate that passed steps
1–5, so policy candidates count too. A policy candidate that is finally
admitted is counted as `admitted_under_fragment_policy`.

The raw fragment numbers of the step-4 candidates are reported per group,
0 to 15, as a diagnostic.

## Decoder oracle in version 2

For every admitted file, tshark 4.2.2 checks two samples:
- the first four admitted fragment-zero reports
- the first four reports admitted under the policy

The match rules are unchanged from version 1. Any mismatch means the
benchmark is not admitted.

## Reproducer

The workflow is `.github/workflows/public-bfi-motion.yml`, run with
`protocol: public-bfi-motion-2`. To run it locally:

```bash
python tools/bfi_learning/download_public_bfi.py --include-heldout --output /private/bfi
python tools/bfi_learning/public_bfi_motion.py --protocol public-bfi-motion-2 \
  --data /private/bfi --report report.md --json report.json
```

## Results

Not yet run.

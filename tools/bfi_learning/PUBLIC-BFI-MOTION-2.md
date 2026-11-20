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

**Run 1 (2026-11-20): benchmark NOT ADMITTED. MEASURED** on public data,
under the policy `csi-bfi-har-complete-mpdu-v1`. Not comparable to version 1.
- Workflow run [44695427917](https://github.com/Justinwold88/RuView/actions/runs/44695427917), commit `98fc1b28`.
- Dataset revision `c3013b3e…`, manifest SHA-256 `c2db11f7…`, Wireshark 4.2.2.

**Every file passed file admission.**
- All 240 files passed integrity, the PCAPNG inventory and lossless conversion.
- No candidate failed the policy's subtype or FCS rules.
- **Decoder oracle:** 1,293 admitted reports from all 240 files matched tshark
  4.2.2 exactly; 381 of them were admitted under the policy.

**The policy admits few of the nonzero-fragment frames.** Almost all of them
have a fragment sibling within 1 s, so they stay quarantined. Of the
nonzero-fragment candidates, 1.2% were admitted in train, 0.3% in
validation and under 0.01% in the held-out groups:

| Group | Admitted reports | Of which under the policy | Quarantined (fragment sibling) | Admitted share |
|---|---|---|---|---|
| train | 29,988 | 4,552 | 376,999 | 7.4% |
| validation | 27,112 | 1,089 | 389,776 | 6.5% |
| heldout_same_device | 66,328 | 40 | 993,804 | 6.3% |
| heldout_other_device_view | 68,707 | 7 | 1,030,372 | 6.3% |

In every group, each raw fragment number from 0 to 15 holds between 6.2% and
6.3% of the candidates. With 16 values that are equally common, almost every
12-bit sequence number has siblings, which is what the sibling rule rejects.
This fits the version 1 observation that the low four bits of Sequence
Control behave like a counter. It is an observation, not a reason to admit
the frames.

**Too few intervals survive.**

| Group | Recordings with an eligible interval | Eligible intervals | Activities with fewer than 2 such recordings |
|---|---|---|---|
| train | 15 of 60 (version 1: 6) | 31 | 17 of 20 (all but A, C and I) |
| validation | 11 of 60 (version 1: 11) | 139 | 19 of 20 (all but J) |
| heldout_same_device | 53 of 60 | 1,065 | 1 (B) |
| heldout_other_device_view | 56 of 60 | 1,109 | 0 |

Every activity needs 2 such recordings in every group, so the benchmark is
not admitted and no detector was scored. The held-out groups were inspected
for quality only.

**What would change it.** Only a different, reviewed dataset policy: one
that treats all 16 bits of Sequence Control as a counter instead of a
sequence and fragment number. That would need another ADR-365 amendment and a
protocol version 3, frozen before its run. Whether to do that is the study
owner's decision. Relaxing the sibling rule or the interval rule inside
version 2 is not allowed.

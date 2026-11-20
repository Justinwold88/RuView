# ADR-427: CSI-BFI-HAR complete-MPDU policy for nonzero fragment bits (amends ADR-365)

- **Status**: proposed (fork; accepted for the fork's public BFI test by the study owner, 2026-11-20)
- **Date**: 2026-11-20
- **Deciders**: RuView maintainers; fork study owner
- **Tags**: bfi, datasets, decoding, fragmentation, benchmarks

## Context

[ADR-365](ADR-365-public-bfi-dataset-and-decoder-contract.md) quarantines
public BFI frames whose Sequence Control fragment number is nonzero "until an
explicit, independently reviewed dataset policy can justify admission". The
[decode audit](../../tools/bfi_learning/PUBLIC-BFI-DECODE.md) allows "a later
separately reviewed dataset-specific adapter". That adapter may accept
already-complete MPDUs under a named policy, kept apart from live capture
semantics, with exact-length and oracle tests showing it cannot admit a
partial body.

**MEASURED, first run of the strict protocol** (`public-bfi-motion-1`,
[results](../../tools/bfi_learning/PUBLIC-BFI-MOTION.md#results)):

- **Integrity:** all 240 pinned files converted losslessly, every frame's FCS
  matched, and no other traffic was present.
- **What the quarantine removed:** 93.75% of the feedback frames in every
  partition. The admitted share was within 0.01 percentage points of 1/16.
  That is what four uniformly distributed bits would give.
- **Effect:** train and validation kept a usable 5-second interval in only 6
  and 11 of 60 recordings, so the benchmark could not be admitted.
- **Frame form:** in the audited sample every such frame is Action No Ack,
  with More Fragments clear and a complete 1,003-byte single-segment report.

A real fragment train would look different: a first fragment with More
Fragments set, then siblings that share one sequence number with rising
fragment numbers. The observed frames do not look like that. They look like a
capture or driver artifact in which the low four bits carry a counter.

## Decision

Define the named policy **`csi-bfi-har-complete-mpdu-v1`**. It applies only
to files from the pinned CSI-BFI-HAR manifest (revision `c3013b3e…`),
processed by `tools/bfi_learning/public_bfi_motion.py` with
`--protocol public-bfi-motion-2`.

A frame with a nonzero fragment number is admitted only if **all** of these
hold. Otherwise it stays quarantined, and the reason is counted:

1. **The usual checks.** It passes every other step that a fragment-zero frame
   must pass:
   - integrity, inventory and lossless conversion
   - radiotap
   - management Action category 21/0
   - ToDS, FromDS, More Fragments, Protected and Order all clear
   - the file's direction
   - `public_bfi_decode.decode_body`, which demands exactly 1,003 bytes,
     VHT MU 3x1, 80 MHz, Ng=1, codebook 1, `first_segment = 1` and
     `remaining_segments = 0`
   - the retry and time-order rules
2. **Subtype.** It is Action No Ack (subtype 14). An Action frame (subtype 13)
   with a nonzero fragment number stays quarantined.
3. **FCS.** Radiotap declares an FCS and it matches a CRC-32 over the
   unmodified MAC bytes. A frame without an FCS stays quarantined, because its
   completeness cannot be shown.
4. **No fragment siblings.** No other candidate frame in the same file, from
   the same transmitter and receiver, shares its 12-bit sequence number with a
   different fragment number within 1 s. Such siblings would indicate real
   fragmentation, so the policy frames in that group stay quarantined. A
   fragment-zero frame keeps its strict-mode handling.
5. **Nothing rewritten.** No header, Sequence Control or FCS byte is changed.
   The raw fragment number is kept, and its distribution is reported per
   partition as a diagnostic.
6. **Oracle.** In every file, the decoder oracle compares the first four
   reports admitted under this policy with tshark 4.2.2, in addition to the
   first four fragment-zero reports. Any mismatch fails benchmark admission.

**What does not change:**

- **The live path.** The live capture path (`tools/bfi`, `prepare_bfi.py`, and
  the Rust sensing server) keeps rejecting nonzero fragment numbers on Action
  frames.
- **Strict mode.** Protocol `public-bfi-motion-1` is unchanged and still
  reproducible with `--protocol public-bfi-motion-1`.
- **The policy's reach.** It reinterprets no other dataset or capture.

The policy does not claim to know what the bits mean. It only shows that each
admitted frame is a single, complete, CRC-valid report with no fragment
siblings. A new protocol version (`public-bfi-motion-2`) adopts it, and that
version is frozen before it runs. Held-out groups are still inspected for
quality only and never scored.

## Consequences

### Positive

- The public train and validation partitions can be tested without weakening
  live validation.
- Every frame admitted this way is complete, CRC-valid, oracle-checked and
  counted separately.

### Negative

- If the bits do encode fragmentation in a way the sibling check misses, the
  policy could admit reports from an unusual capture. The exact-length,
  segment and FCS checks keep those reports complete. Only their timing
  meaning would be in doubt.
- Results under this policy are labelled with the policy name, and are not
  comparable to strict-mode results.

### Neutral

- This is a dataset adapter for offline research. It is not a
  production-integration decision.

## Validation

- `tools/bfi_learning/test_public_bfi_motion.py` covers the policy on SYNTHETIC
  frames:
  - admitted frames and sibling rejection
  - the subtype rule and frames without an FCS
  - a corrupted FCS
  - truncated and extended bodies
  - the strict mode left unchanged
- The policy oracle uses tshark 4.2.2 where it is installed.
- The [protocol v2](../../tools/bfi_learning/PUBLIC-BFI-MOTION-2.md) run in
  `public-bfi-motion.yml` reports the policy counts next to the census.

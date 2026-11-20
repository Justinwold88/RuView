# ADR-365: Public BFI dataset and decoder contract

- **Status**: proposed
- **Date**: 2026-09-29
- **Deciders**: RuView maintainers; acceptance pending
- **Tags**: bfi, datasets, decoding, provenance, benchmarks

## Context

Public activity recordings offer a separate route to BFI model development
while local capture continuity is insufficient. They introduce different frame
formats, collection assumptions and licenses. Public HAR activity labels do
not reproduce KIT's person-identification experiment.

**MEASURED:** the pinned CSI-BFI-HAR train/validation download passed exact
size/hash checks for 120 captures plus its license card. A separate body decoder
matched 13,104 quantized angles across 14 sample bodies against tshark 4.2.2 in
two independent executions. Whole-packet admission and activity training remain
incomplete. See [results](../../tools/bfi_learning/RESULTS.md) and the
[decoder audit and reproducer](../../tools/bfi_learning/PUBLIC-BFI-DECODE.md).

## Decision

### Pin provenance and partitions

Use the [author dataset](https://huggingface.co/datasets/foysalhaque/CSI-BFI-HAR-Dataset/tree/c3013b3edc5a563a9cf6759f5d464a9d9b6f456a)
at revision `c3013b3edc5a563a9cf6759f5d464a9d9b6f456a`. The
[manifest](../../tools/bfi_learning/manifests/csi-bfi-har-c3013b3-subset.json)
pins URLs, lengths, SHA-256, labels and split membership. Preserve the pinned
dataset card's GPL-3.0 declaration and attribution separately from code licenses.

| Partition | Author directory | Domain |
| --- | --- | --- |
| Training | HAR-1/BFI/M1 | Day 1, kitchen |
| Validation | HAR-3/BFI/M1 | Day 3, classroom |
| Held out | HAR-5/BFI/M1 and M2 | Day 5, living room; two views |

Keep all views of one event in the same partition. Day and room are confounded;
M1/M2 labels do not establish heterogeneous-chipset transfer. The selected
filenames cover A–T activities and P1–P3 participants.

Default downloads exclude held-out views. The bounded
[downloader](../../tools/bfi_learning/download_public_bfi.py) verifies exact
bytes before atomic completion, rehashes resumed files, retains failed partials
and enforces transfer deadlines and byte limits. Data stays outside Git;
downloaded code is never executed by the downloader.

### Keep body decoding separate from packet admission

[public_bfi_decode.py](../../tools/bfi_learning/public_bfi_decode.py) accepts
only the audited VHT MU 3x1, 80 MHz, Ng=1, codebook-1 complete action body:
1,003 bytes containing one SNR byte, 936 angle bytes and 61 opaque MU-exclusive
bytes. It exposes quantized/radian angles and tone ordinals. Frequency indices,
exclusive-field interpretation and validated matrix reconstruction remain
separate acceptance work.

Twelve sample frames have nonzero fragment-number bits. Successful tshark
decoding and valid CRCs do not resolve their semantics. Retain live MAC guards;
quarantine these frames until an explicit, independently reviewed dataset
policy can justify admission. Require lossless pcapng conversion evidence for
integer timestamps, packet lengths, link types and all original packet bytes.
Do not silently strip headers or rewrite fragment fields to gain admission.

The natural circular representation needs 1,404 features, exceeding the current
trainer's 1,024-feature limit. Approve a bounded representation or revised limit
with allocation tests before integration; do not truncate tones implicitly.

### Admit recording quality before scoring models

Inventory every manifest file, including tiny or unusable traces. Freeze a
five-second observed-interval rule: at least 20 valid unique reports, with no
inter-report gap above one second and no interpolation. Require at least two
independent recordings per activity in each partition/view group that supply
an eligible interval. Missing A–T coverage fails benchmark admission.
Before implementing the census, also freeze interval anchoring, stride and
leading/trailing gap rules: an inter-report gap bound alone does not bound
silence at interval edges. These details remain required acceptance work.

Public files lack trusted recorder start/end manifests. Observed packet spans
cannot establish unobserved leading/trailing coverage. Report rejection reasons,
eligible recording/time coverage and accuracy together. Freeze event/file groups
before generating windows; inspect held-out quality without model selection
against held-out outcomes.

## Alternatives

Reusing the HE/SU parser without format admission, weakening live fragment
guards, or silently dropping sparse classes would change the task while hiding
the change. Keep this adapter explicit and fail incomplete benchmark coverage.

## Consequences

### Positive

Data provenance, protocol support and score coverage remain independently
auditable. New public formats cannot silently weaken live capture validation.

### Negative

Sparse files or unresolved framing may prevent a benchmark despite a successful
download. Format and representation work must precede model optimization.

### Neutral

This is offline research/reference tooling. Production integration must reuse
the Rust protocols in [ADR-291](ADR-291-public-benchmark-evaluation-harness.md)
and scorecards in [ADR-317](ADR-317-benchmark-multi-domain-scorecard.md); this ADR
does not establish a second production trainer or an integrated benchmark score.

## Validation and links

Use `test_download_public_bfi.py` and `test_public_bfi_decode.py` under
`tools/bfi_learning`, then the full packet oracle and quality census specified
in the [decode audit](../../tools/bfi_learning/PUBLIC-BFI-DECODE.md) and
[benchmark protocol](../../tools/bfi_learning/RESEARCH-NEXT.md).

- Depends on [ADR-364](ADR-364-bfi-capture-admission-and-continuity.md)'s distinction
  between packet validity and coverage.
- Preserves [ADR-120](ADR-120-bfld-privacy-class-and-hash-rotation.md).
- [ADR-119](ADR-119-bfld-frame-format-and-wire-protocol.md) defines a BFLD envelope;
  that envelope does not replace raw 802.11 decoding.

## Amendment 1 — Dataset policy for nonzero Sequence Control low bits

- **Date**: 2026-11-20
- **Status**: proposed. Requested by the fork owner after the first strict
  census. It is not a maintainer acceptance, and upstream review is pending.
- **Scope**: the public CSI-BFI-HAR adapter (`tools/bfi_learning/public_bfi_motion.py`)
  at revision `c3013b3e…` only. Live capture, `prepare_bfi.py`, the sensing
  server and every other adapter keep rejecting nonzero fragment numbers.

### Why

The first strict census (protocol `public-bfi-motion-1`, fork run
44667646386) quarantined every frame with nonzero Sequence Control low bits.
**MEASURED**:
- 93.75% of BFI action frames in every partition/view group were quarantined.
- All 240 files passed hashes, inventory, lossless conversion, radiotap/FCS
  and the MAC guards. No frame had More Fragments set.
- The admitted share was 1/16 in every group to within 0.01 percentage points.

Four uniformly distributed bits would give exactly that share. In these
captures the bits behave like a counter, not like fragment numbers of a
fragmented MPDU. The remaining reports left train and validation with eligible
intervals in only 6 and 11 of 60 recordings, so the benchmark was not admitted.

This is evidence about this dataset only. It does not show the bits are
normatively reserved for Action No Ack frames; that question stays open, as
the decode audit records.

### Policy `csi-bfi-har-c3013b3-seqctl-v1`

A feedback frame whose Sequence Control low four bits are nonzero is admitted
only if **all** of these hold, in addition to every rule that applies to
fragment-zero frames:

1. **Integrity:** radiotap declares an FCS, and the FCS matches a CRC-32 over
   the whole MAC frame (`strip_radiotap` removed and verified it).
2. **No fragmentation:** More Fragments is clear. This is already a live MAC
   guard.
3. **Complete:** the MAC frame is exactly 24 + 1,003 bytes, one complete VHT MU
   3x1 body. `decode_body` also requires a single, complete feedback segment
   (remaining segments 0, first segment 1).
4. **Nothing rewritten:** the bytes are never changed. The low-bit value is
   counted per group (`seqctl_low_bits_NN`) and is never used as a sequence or
   loss counter.
5. **Oracle:** in every admitted file, up to four policy-admitted reports are
   compared with tshark 4.2.2 under default preferences, alongside up to four
   fragment-zero reports:
   - All control fields, the SNR code and all 936 angle integers must match.
   - Each report is a separate capture record, so tshark decodes it from that
     frame alone.
   - With 802.11 reassembly turned off, tshark 4.2.2 does not dissect the body
     of any nonzero-fragment frame, complete or not, so that setting cannot be
     the oracle.

A frame that fails rule 1 or rule 3 stays quarantined, with its own reason.

**Partial bodies.** Every fragment of a fragmented MPDU except the last has
More Fragments set, so it is rejected. A last fragment would pass only if it
happened to begin with a valid VHT category and action, carried valid control
bits and was exactly 1,003 bytes long. The census counts each rejection
reason and the oracle compares samples in every file, so a systematic case
would show there.

### Consequences

- **A new protocol version.** The census and scoring rules are unchanged. The
  frozen protocol `public-bfi-motion-2` differs from version 1 only in this
  admission step, and was written after version 1's census had been seen.
  Nothing else was tuned on those results.
- **Held-out groups** are still inspected for quality only.
- **Live paths are unchanged.** A future live adapter that wants the same
  behaviour needs its own amendment.

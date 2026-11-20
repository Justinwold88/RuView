# Public BFI motion test: frozen protocol (plan item 1.6)

Status: **protocol frozen before any data was downloaded, inspected or scored.**
Results go in a separate section added after the first run. Nothing above that
section may change once the first run has been dispatched; a change needs a new
protocol version and a new run.

This applies [ADR-365](../../docs/adr/ADR-365-public-bfi-dataset-and-decoder-contract.md)
and the [decode audit](PUBLIC-BFI-DECODE.md) strictly. ADR-365 leaves one rule
to be frozen before the census: interval anchoring, stride and leading/trailing
gap rules. They are frozen below.

## Question

On public data, can a simple statistic of the beamforming feedback angles tell
activities with sustained large body or arm motion from fine-motor or
stationary ones?

This is **not** a presence test. Every recording in the dataset has a person in
the room, so presence cannot be scored. It is not a claim about real homes, about
other rooms or devices, or about ESP32 CSI.

## Data (unchanged from ADR-365)

- **Source:** dataset `foysalhaque/CSI-BFI-HAR-Dataset`, revision
  `c3013b3edc5a563a9cf6759f5d464a9d9b6f456a`. Its GPL-3.0 dataset card is kept
  with the data.
- **Manifest:** only the pinned `manifests/csi-bfi-har-c3013b3-subset.json`,
  fetched with `download_public_bfi.py`, which verifies exact length and SHA-256.
  The scorer re-hashes every file it reads against the manifest.
- **Groups:** all four partition/view groups are downloaded, because ADR-365
  requires the quality census in each one.

  | Group | Author directory | Used for |
  |---|---|---|
  | train | HAR-1/BFI/M1 | census, threshold fitting |
  | validation | HAR-3/BFI/M1 | census, scoring |
  | heldout_same_device | HAR-5/BFI/M1 | census only |
  | heldout_other_device_view | HAR-5/BFI/M2 | census only |

- **Held-out data is never scored.** Only its quality is inspected, as ADR-365
  allows ("inspect held-out quality without model selection against held-out
  outcomes"). Held-out scoring is left for the frozen model comparison in
  `RESEARCH-NEXT.md`.
- **No CSI.** The pinned protocol forbids expanding the selection to CSI, so the
  BFI-versus-CSI comparison the improvement plan asked for cannot be made under
  ADR-365 as written. Adding CSI needs its own pinned manifest and an ADR
  amendment.
- **Data stays on the runner.** Packets are never committed or uploaded. Only
  aggregate numbers leave the runner. No addresses, angles or packet bytes are
  written to the report.

## File admission

Each file is admitted only if all of these hold. Otherwise it is rejected under
the first failing reason, and it stays in the census.

1. **Integrity:** its bytes match the manifest length and SHA-256, and it is at
   most 64 MiB.
2. **PCAPNG inventory.** Checked by a narrow reader that converts nothing:
   - one section
   - known byte order
   - matching block lengths
   - exactly one interface, with link type 127 (radiotap)
   - a timestamp resolution that is an exact whole number of nanoseconds per
     tick (10^-0 to 10^-9 seconds), and no timestamp offset
   - only Enhanced Packet Blocks for packets, each with captured length at most
     the original length and at most the snap length

   Simple and obsolete packet blocks are rejected because they carry no
   timestamp or interface clock.
3. **Lossless conversion:**
   - The file is converted with `editcap -F nsecpcap`, using Wireshark 4.2.2 exactly.
   - The output must agree with the inventory on packet count, integer
     nanosecond timestamps, captured lengths, original lengths and every
     packet's bytes.
   - The editcap version and argv, and the hashes of both files, go in the
     receipt.
   - Admission reads the converted file. The converted file is deleted after use.

## Packet admission

Each packet is classified in this order. Its first failing step is counted.

1. **Not truncated:** captured length equals original length.
2. **Radiotap and FCS:** the radiotap header is valid and not marked bad-FCS. If
   radiotap says an FCS is present, it must match a CRC-32 over the MAC frame.
   This is the shared `tools/bfi/bfi_decode.py::strip_radiotap`.
3. **Candidate feedback:**
   - protocol version 0
   - management type
   - subtype Action (13) or Action No Ack (14)
   - at least 26 bytes long
   - body starting with category 21, action 0 (VHT compressed beamforming)

   Any other packet is counted as other traffic, not as a rejection.
4. **Live MAC guards:** ToDS, FromDS, More Fragments, Protected and Order are all
   clear.
5. **Fragment quarantine.** As ADR-365 requires, a nonzero fragment number (low
   four bits of Sequence Control) is **quarantined**, not admitted. Headers are
   not stripped and fields are not rewritten.
6. **Direction:**
   - The transmitter/receiver address pair must equal the file's most common pair
     among candidates that passed steps 1–5.
   - This stands in for the live path's configured AP/client check, which public
     files cannot supply.
   - Ties go to the pair seen first.
7. **Body:** the 1,003-byte action body passes `public_bfi_decode.decode_body`
   (VHT MU 3x1, 80 MHz, Ng=1, codebook 1, one complete segment). Anything else is
   counted as an unsupported format.
8. **Retry duplicate:** dropped if the Retry bit is set and an admitted report
   with the same Sequence Control and body is less than 1 s older. Identical
   bodies without Retry are counted as duplicate payloads but not dropped.
9. **Time order:** the timestamp is strictly later than the previous admitted
   report's.

A packet that passes all nine steps is an **admitted report**.

## Independent decoder oracle

The decode audit requires an independent check on fragment-zero frames before
import:

- **Sample:** the first four admitted reports of every admitted file (or all of
  them, if there are fewer).
- **Comparison:** each is compared with `tshark -T pdml` 4.2.2, run with no
  preference overrides. These must match exactly:
  - the ten MIMO control fields, including the sounding token
  - the signed average-SNR code
  - all 936 angle integers, in tshark's φ11, φ21, ψ21, ψ31 order
- **Not validated:** tshark's subcarrier index labels are sequential, so the
  frequency axis is not validated. The detector below averages over all tones and
  does not depend on tone positions.
- **Failure:** any mismatch, malformed-packet flag or oracle error means **the
  benchmark is not admitted.**

## Observed intervals (frozen here)

- **Anchor:** each admitted file is tiled from its first admitted report's
  timestamp `t0`, using the intervals `[t0 + 5k, t0 + 5(k+1))` seconds, for
  k = 0 up to and including the interval that holds the last admitted report.
- **Stride:** 5 s; intervals do not overlap.
- **Eligible** when all of these hold (gaps use integer nanoseconds, and
  "at most" includes equality):
  - at least 20 admitted reports fall in the interval
  - no gap between consecutive admitted reports inside it exceeds 1 s
  - the gap from the interval's start to its first report is at most 1 s
  - the gap from its last report to the interval's end is at most 1 s
- **No interpolation.** Observed spans run from the first to the last admitted
  report. They are not recorder start/end times.

## Benchmark admission

For **each of the four groups** and each activity A–T, at least **two different
recordings** must supply at least one eligible interval. The benchmark is not
admitted, and no detector is scored, if either of these happens:

- any activity in any group falls short
- the decoder oracle fails

The census is then the result. Nothing is relabelled and the label set is never
shrunk.

## Labels (frozen here)

The author's activity codes are grouped by whether the activity involves
sustained large body or arm motion:

| Group | Codes |
|---|---|
| **Moving** | A jogging, B clapping, C push forward, D boxing, G rotating, K waving, L walking, O hands up-down, Q side bends |
| **Still / fine-motor** | E writing, F brushing teeth, H standing, I eating, J reading a book, M browsing phone, N drinking, P phone call, R check wrist, S washing hands, T browsing laptop |

The grouping is a judgement call; clapping and waving are arm-only. Per-activity
statistics are reported too, so readers can regroup.

## Detector and controls (scored only if admitted)

- **BFI motion feature.**
  - For each pair of consecutive admitted reports inside an eligible interval:
    - Take each tone's absolute change in φ11 and φ21 (circular difference) and
      in ψ21 and ψ31 (plain difference).
    - Average over all 234 tones and the four angles. Angles are quantization
      bin centres in radians.
  - The interval's feature is the median over its pairs.
- **Timing-only control:** the number of admitted reports in the interval.
- **Constant-class control:** always predict the group that has the most train
  intervals.
- **Threshold:** fitted on **train intervals only**:
  - Candidates are below-minimum, above-maximum, and the midpoints between
    sorted distinct feature values.
  - Both directions are tried. The pair with the highest balanced accuracy wins.
  - Ties go to the lower threshold, then to "greater means moving".
  - The same rule is used for the timing-only control.
- **Scoring on validation:**
  - interval-level balanced accuracy and confusion counts, with moving as the
    positive class
  - recording-level balanced accuracy and confusion counts, where a recording's
    feature is the median of its intervals' features and the interval threshold
    is reused
  - coverage: admitted files, eligible intervals and eligible seconds per group
- **Per activity:** the median feature and the interval count, on train and
  validation.
- **No model selection happens on validation.**

## Reporting

- **Tags:** all numbers are **MEASURED on public data**. Each is tagged with:
  - the dataset revision
  - the manifest SHA-256
  - this repository's commit
  - the protocol version (`public-bfi-motion-1`)
  - the workflow run
- **Comparison:** the [SYNTHETIC scenario harness](../../v2/crates/wifi-densepose-sensing-server/src/scenario_harness.rs)
  numbers are a different kind of evidence. Do not compare the two directly.
- **Reproducer:** `.github/workflows/public-bfi-motion.yml` (manual dispatch).
  To run it locally (needs Wireshark 4.2.2 and about 3.4 GB of disk):

  ```bash
  python tools/bfi_learning/download_public_bfi.py --include-heldout --output /private/bfi
  python tools/bfi_learning/public_bfi_motion.py --data /private/bfi --report report.md --json report.json
  ```

## Results

**Run 1 (2026-11-20): benchmark NOT ADMITTED. MEASURED** on public data.
- Workflow run [44667646386](https://github.com/Justinwold88/RuView/actions/runs/44667646386), commit `06a68c72`.
- The rerun [44672983475](https://github.com/Justinwold88/RuView/actions/runs/44672983475) at commit `53537b57` only adds printing the report to the log. Its census is identical.
- Dataset revision `c3013b3e…`, manifest SHA-256 `c2db11f7…`, Wireshark 4.2.2.

**Every file passed file admission.**
- All 240 files matched their manifest hashes and passed the PCAPNG inventory.
- Every file converted losslessly with editcap.
- No packet failed radiotap/FCS or the MAC guards, and no file held other traffic.
- **Decoder oracle:** 912 admitted reports from 233 files matched tshark 4.2.2 exactly (controls, SNR and all angle integers). The other 7 files had no admitted report.

**The fragment quarantine removes about 94% of reports.**

| Group | Admitted reports | Quarantined (nonzero fragment number) | Admitted share |
|---|---|---|---|
| train | 25,436 | 381,551 | 6.2% |
| validation | 26,023 | 390,865 | 6.2% |
| heldout_same_device | 66,288 | 993,844 | 6.3% |
| heldout_other_device_view | 68,700 | 1,030,379 | 6.3% |

In every group the admitted share is within 0.01 percentage points of 1/16.
That is what four uniformly distributed bits would give: the low four bits of
Sequence Control behave like a counter here, not like fragment numbers. This
is an observation for a future dataset policy, not a reason to admit the
frames. ADR-365 still requires a reviewed policy first.

**Too few intervals survive.** With what is left, few 5-second intervals keep 20 reports without a gap above 1 s:

| Group | Recordings with an eligible interval | Eligible intervals | Activities with fewer than 2 such recordings |
|---|---|---|---|
| train | 6 of 60 | 12 | 19 of 20 (all but I) |
| validation | 11 of 60 | 140 | 19 of 20 (all but J) |
| heldout_same_device | 53 of 60 | 1,060 | 1 (B) |
| heldout_other_device_view | 55 of 60 | 1,107 | 0 |

Every activity needs 2 such recordings in every group, so the benchmark is not
admitted and no detector was scored. This is the result ADR-365 asks for
rather than a failure to work around. The label set was not shrunk, and the
quarantine was not relaxed.

**What would change it.**
- **An explicit dataset policy for the nonzero-fragment frames.** The decode
  audit allows "a later separately reviewed dataset-specific adapter" that
  accepts already-complete MPDUs, under a named policy with an oracle for the
  original bytes. That needs an ADR-365 amendment, then a new protocol version
  and a new run. The held-out groups would still not be scored until that
  protocol freezes.
- **Not by changing the interval rule.** Even the day-5 groups, which are
  denser, lose some activities, and train and validation would still be far
  short.

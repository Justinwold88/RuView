"""SYNTHETIC tests for the public BFI motion test. No network, no public data."""
import math
import shutil
import struct
import tempfile
import unittest
import zlib
from pathlib import Path

import public_bfi_motion as motion

AP, STA, OTHER = bytes.fromhex("020000000001"), bytes.fromhex("020000000002"), bytes.fromhex("020000000003")
NS = motion.NS
HAVE_WIRESHARK = all(shutil.which(tool) for tool in ("tshark", "editcap"))


def wireshark_ok():
    if not HAVE_WIRESHARK:
        return False
    try:
        motion.tool_version(shutil.which("tshark"), "TShark")
        motion.tool_version(shutil.which("editcap"), "Editcap")
        return True
    except motion.DecodeError:
        return False


WIRESHARK = wireshark_ok()


def rows_for(seed):
    return [((seed * 7 + i * 3) % 512, (seed * 5 + 511 - i) % 512, (seed + i) % 128, (seed * 3 + 127 - i) % 128)
            for i in range(234)]


def body(rows, token=9, snr=-12, codebook=1):
    control = (2 << 3) | (2 << 6) | (codebook << 10) | (1 << 11) | (1 << 15) | (token << 18)
    angles = b"".join(struct.pack("<I", a | (b << 9) | (c << 18) | (d << 25)) for a, b, c, d in rows)
    return b"\x15\x00" + control.to_bytes(3, "little") + struct.pack("b", snr) + angles + bytes(range(61))


def packet(payload, seq=0, frag=0, fc=0x00e0, ta=STA, ra=AP, fcs=True, corrupt_fcs=False):
    frame = struct.pack("<HH", fc, 0) + ra + ta + ra + struct.pack("<H", (seq << 4) | frag) + payload
    if not fcs:
        return struct.pack("<BBHI", 0, 0, 8, 0) + frame
    checksum = zlib.crc32(frame) ^ (1 if corrupt_fcs else 0)
    return struct.pack("<BBHIB", 0, 0, 9, 0x2, 0x10) + frame + struct.pack("<I", checksum)


def block(kind, content, endian="<"):
    padded = content + bytes(-len(content) % 4)
    length = 12 + len(padded)
    return struct.pack(endian + "II", kind, length) + padded + struct.pack(endian + "I", length)


def option(code, value, endian="<"):
    return struct.pack(endian + "HH", code, len(value)) + value + bytes(-len(value) % 4)


def pcapng(packets, endian="<", tsresol=9, snaplen=0, extra_idb=False, tsoffset=None, linktype=127):
    options = b""
    if tsresol is not None:
        options += option(9, bytes([tsresol]), endian)
    if tsoffset is not None:
        options += option(14, struct.pack(endian + "q", tsoffset), endian)
    options += option(0, b"", endian) if options else b""
    out = [block(motion.SHB, struct.pack(endian + "IHHq", 0x1A2B3C4D, 1, 0, -1), endian),
           block(motion.IDB, struct.pack(endian + "HHI", linktype, 0, snaplen) + options, endian)]
    if extra_idb:
        out.append(block(motion.IDB, struct.pack(endian + "HHI", 127, 0, 0), endian))
    for ticks, data, *original in packets:
        original = original[0] if original else len(data)
        out.append(block(motion.EPB, struct.pack(endian + "IIIII", 0, ticks >> 32, ticks & 0xFFFFFFFF,
                                                 len(data), original) + data, endian))
    return b"".join(out)


def recording(seconds=20, rate=10, moving=True, start_ns=1_700_000_000 * NS, counter_bits=False,
              counter16=False, first=0):
    """Evenly spaced feedback; 'moving' changes the angles between reports.

    counter_bits fills the fragment-number bits with a 0-15 counter.
    counter16 makes all 16 Sequence Control bits one counter that steps by 1 per report,
    starting at `first`: the 12-bit sequence number then changes only every 16 reports.
    """
    out = []
    for j in range(seconds * rate):
        rows = rows_for(j if moving else 0)
        if counter16:
            value = (first + j) % 65536
            seq, frag = value >> 4, value & 0xF
        else:
            seq, frag = j % 4096, j % 16 if counter_bits else 0
        out.append((start_ns + j * NS // rate, packet(body(rows), seq=seq, frag=frag)))
    return out


class PcapngTests(unittest.TestCase):
    def test_round_trip_both_byte_orders_and_resolutions(self):
        data = packet(body(rows_for(1)))
        for endian in "<>":
            inventory, packets = motion.pcapng_packets(pcapng([(123_456_789_012, data)], endian=endian))
            self.assertEqual(packets, [(123_456_789_012, len(data), len(data), data)])
            self.assertEqual(inventory["linktype"], 127)
            self.assertEqual(inventory["tsresol_exponent"], 9)
        _, packets = motion.pcapng_packets(pcapng([(5, data)], tsresol=None))
        self.assertEqual(packets[0][0], 5000)  # default microseconds
        _, packets = motion.pcapng_packets(pcapng([(5, data)], tsresol=0))
        self.assertEqual(packets[0][0], 5 * NS)

    def test_fails_closed(self):
        data = packet(body(rows_for(1)))
        good = pcapng([(1, data)])
        cases = {
            "pcapng_multiple_interfaces": pcapng([(1, data)], extra_idb=True),
            "pcapng_linktype_not_radiotap": pcapng([(1, data)], linktype=105),
            "pcapng_timestamp_offset_present": pcapng([(1, data)], tsoffset=7),
            "pcapng_timestamp_resolution_not_whole_ns": pcapng([(1, data)], tsresol=10),
            "pcapng_trailing_length_mismatch": good[:-4] + struct.pack("<I", 12),
            "pcapng_block_truncated": good + b"\x06\x00\x00",
            "pcapng_invalid_packet_lengths": pcapng([(1, data, len(data) - 1)]),
            "pcapng_multiple_sections": good + good,
            "pcapng_packet_block_without_clock": good + block(motion.SPB, struct.pack("<I", len(data)) + data),
            "pcapng_unknown_byte_order": good[:8] + b"\x00\x00\x00\x00" + good[12:],
        }
        for reason, raw in cases.items():
            with self.subTest(reason=reason), self.assertRaises(motion.DecodeError) as caught:
                motion.pcapng_packets(raw)
            self.assertEqual(caught.exception.reason, reason)
        self.assertEqual(motion.pcapng_packets(pcapng([(1, data)], tsoffset=0))[1][0][0], 1)
        base2 = pcapng([(1, data)], tsresol=0x89)
        with self.assertRaises(motion.DecodeError):
            motion.pcapng_packets(base2)

    def test_lossless_comparison_detects_any_difference(self):
        a = [(1, 3, 3, b"abc"), (2, 3, 3, b"def")]
        self.assertTrue(motion.verify_lossless(a, list(a)))
        for changed, reason in (([(1, 3, 3, b"abc")], "conversion_packet_count_mismatch"),
                                ([(1, 3, 3, b"abc"), (3, 3, 3, b"def")], "conversion_timestamp_or_length_mismatch"),
                                ([(1, 3, 3, b"abc"), (2, 3, 3, b"deF")], "conversion_packet_bytes_mismatch")):
            with self.subTest(reason=reason), self.assertRaises(motion.DecodeError) as caught:
                motion.verify_lossless(a, changed)
            self.assertEqual(caught.exception.reason, reason)

    def test_nsec_pcap_reader(self):
        header = struct.pack("<IHHIIII", 0xa1b23c4d, 2, 4, 0, 0, 262144, 127)
        record = struct.pack("<IIII", 7, 999_999_999, 3, 3) + b"xyz"
        self.assertEqual(motion.nsec_pcap_packets(header + record), [(7 * NS + 999_999_999, 3, 3, b"xyz")])
        usec = struct.pack("<IHHIIII", 0xa1b2c3d4, 2, 4, 0, 0, 65535, 127)
        with self.assertRaises(motion.DecodeError):
            motion.nsec_pcap_packets(usec + record)
        with self.assertRaises(motion.DecodeError):
            motion.nsec_pcap_packets(header + struct.pack("<IIII", 7, NS, 3, 3) + b"xyz")


class AdmissionTests(unittest.TestCase):
    def test_every_step_counts_its_packets(self):
        base = 10 * NS
        packets = [
            (base + 0, packet(body(rows_for(0)), seq=1)),                              # admitted
            (base + 1, packet(body(rows_for(1)), seq=2)),                              # admitted
            (base + 2, packet(body(rows_for(1)), seq=2, fc=0x08e0)),                   # retry duplicate
            (base + 3, packet(body(rows_for(1)), seq=3)),                              # admitted, dup payload
            (base + 4, packet(body(rows_for(2)), seq=4, frag=5)),                      # quarantined
            (base + 5, packet(body(rows_for(2)), seq=5, corrupt_fcs=True)),            # bad FCS
            (base + 6, packet(b"\x08\x00" + bytes(10), fc=0x0088)),                   # data: other traffic
            (base + 7, packet(b"\x03\x00" + bytes(10))),                               # other action category
            (base + 8, packet(body(rows_for(3)), seq=6, ta=OTHER)),                    # other direction
            (base + 9, packet(body(rows_for(3), codebook=0), seq=7)),                  # unsupported tuple
            (base + 10, packet(body(rows_for(3)) + b"\x00", seq=8)),                   # wrong length
            (base + 11, packet(body(rows_for(3)), seq=9, fc=0x01e0)),                  # ToDS set
            (base + 3, packet(body(rows_for(4)), seq=10)),                             # not later in time
            (base + 12, packet(body(rows_for(5)), seq=11, fcs=False)),                 # admitted, no FCS
        ]
        raw = [(t, len(p), len(p), p) for t, p in packets] + [(base + 13, 5, 9, b"short")]
        counts, times, changes, samples = motion.admit_packets(raw)
        self.assertEqual(counts["admitted_reports"], 4)
        self.assertEqual(times, [base, base + 1, base + 3, base + 12])
        expected = {"dropped_retry_duplicate": 1, "duplicate_payload_admitted": 1,
                    "quarantined_nonzero_fragment": 1, "rejected_fcs_mismatch": 1, "other_traffic": 2,
                    "rejected_other_direction": 1, "rejected_unsupported_tuple": 1, "rejected_body_length": 1,
                    "rejected_mac_guard_flags": 1, "rejected_nonincreasing_timestamp": 1,
                    "rejected_snaplen_truncated": 1}
        for key, value in expected.items():
            self.assertEqual(counts[key], value, key)
        self.assertIsNone(changes[0])
        self.assertEqual(changes[2], 0.0)
        self.assertEqual(len(samples), 4)

    def test_most_common_pair_wins_and_ties_go_to_first_seen(self):
        packets = [(i, packet(body(rows_for(i)), seq=i, ta=STA if i in (1, 3) else OTHER)) for i in range(4)]
        counts, times, _, _ = motion.admit_packets([(t, len(p), len(p), p) for t, p in packets])
        self.assertEqual(times, [0, 2])
        self.assertEqual(counts["rejected_other_direction"], 2)

    def test_radians_match_the_reviewed_decoder(self):
        report = motion.public_bfi_decode.decode_body(body(rows_for(3)))
        self.assertTrue((abs(motion.radians(report["quantized"]) - report["radians"]) < 1e-15).all())

    def test_circular_phi_difference(self):
        a = motion.radians([(0, 0, 0, 0)] * 234)
        b = motion.radians([(511, 0, 0, 0)] * 234)
        self.assertAlmostEqual(motion.angle_change(a, b), (2 * math.pi / 512) / 4)
        c = motion.radians([(0, 0, 127, 0)] * 234)
        self.assertAlmostEqual(motion.angle_change(a, c), (127 * 2 * math.pi / 512) / 4)


class FragmentPolicyTests(unittest.TestCase):
    """ADR-427 csi-bfi-har-complete-mpdu-v1, used only by protocol public-bfi-motion-2."""

    def admit(self, frames, policy=True):
        raw = [(t, len(p), len(p), p) for t, p in frames]
        return motion.admit_packets(raw, fragment_policy=policy)

    def test_strict_mode_is_unchanged(self):
        counts, times, _, _ = self.admit(recording(seconds=2, counter_bits=True), policy=False)
        self.assertEqual((counts["admitted_reports"], counts["quarantined_nonzero_fragment"]), (2, 18))
        self.assertNotIn("fragment_number_00", counts)

    def test_policy_admits_complete_crc_checked_action_no_ack_frames(self):
        counts, times, _, samples = self.admit(recording(seconds=2, counter_bits=True))
        self.assertEqual(counts["admitted_reports"], 20)
        self.assertEqual(counts["admitted_under_fragment_policy"], 18)
        self.assertEqual(counts["fragment_number_00"], 2)
        self.assertEqual(counts["fragment_number_15"], 1)
        frags = [motion.decoder.strip_radiotap(p)[22] & 0x0F for p, _ in samples]
        self.assertEqual(frags, [0, 0, 1, 2, 3, 4])  # 2 fragment-zero reports, then 4 policy reports

    def test_policy_refusals(self):
        base = 10 * NS
        frames = [
            (base, packet(body(rows_for(1)), seq=1, frag=3)),                          # admitted
            (base + 1, packet(body(rows_for(2)), seq=2, frag=3, fc=0x00d0)),            # Action, not No Ack
            (base + 2, packet(body(rows_for(3)), seq=3, frag=3, fcs=False)),            # no FCS
            (base + 3, packet(body(rows_for(4)), seq=4, frag=3, corrupt_fcs=True)),     # bad FCS
            (base + 4, packet(body(rows_for(5)) + b"\x00", seq=5, frag=3)),            # extended body
            (base + 5, packet(body(rows_for(6))[:-1], seq=6, frag=3)),                  # truncated body
            (base + 6, packet(body(rows_for(7)), seq=7, frag=3, fc=0x04e0)),            # More Fragments set
        ]
        counts, times, _, _ = self.admit(frames)
        self.assertEqual(times, [base])
        expected = {"quarantined_policy_not_action_no_ack": 1, "quarantined_policy_no_fcs": 1,
                    "rejected_fcs_mismatch": 1, "rejected_body_length": 2, "rejected_mac_guard_flags": 1,
                    "admitted_under_fragment_policy": 1}
        for key, value in expected.items():
            self.assertEqual(counts[key], value, key)

    def test_fragment_siblings_stay_quarantined(self):
        base = 10 * NS
        frames = [
            (base, packet(body(rows_for(1)), seq=9, frag=1)),
            (base + NS // 2, packet(body(rows_for(2)), seq=9, frag=2)),    # sibling of the first
            (base + NS // 2 + 1, packet(body(rows_for(3)), seq=9, frag=0)),  # fragment zero: v1 rules
            (base + 5 * NS, packet(body(rows_for(4)), seq=9, frag=3)),     # same seq, 4.5 s later: not a sibling
        ]
        counts, times, _, _ = self.admit(frames)
        self.assertEqual(counts["quarantined_fragment_sibling"], 2)
        self.assertEqual(times, [base + NS // 2 + 1, base + 5 * NS])


class CounterPolicyTests(unittest.TestCase):
    """ADR-428 csi-bfi-har-seqctl-counter-v1, used only by protocol public-bfi-motion-3."""

    def admit(self, frames, **policy):
        raw = [(t, len(p), len(p), p) for t, p in frames]
        return motion.admit_packets(raw, **policy)

    def test_a_16_bit_counter_is_admitted_whole(self):
        frames = recording(seconds=4, counter16=True, first=65530)  # wraps past 65535
        strict, strict_times, _, _ = self.admit(frames)
        siblings, sibling_times, _, _ = self.admit(frames, fragment_policy=True)
        counts, times, _, samples = self.admit(frames, counter_policy=True)
        self.assertEqual(len(strict_times), 3)  # only the values whose low four bits are zero
        self.assertEqual(len(sibling_times), 3)  # ADR-427 rejects every policy frame as a sibling
        self.assertEqual(siblings["quarantined_fragment_sibling"], 37)
        self.assertEqual(counts["admitted_reports"], 40)
        self.assertEqual(counts["admitted_under_fragment_policy"], 37)
        self.assertEqual(counts["counter_step_1"], 39)
        self.assertEqual(counts["quarantined_seqctl_duplicate"], 0)
        frags = [motion.decoder.strip_radiotap(p)[22] & 0x0F for p, _ in samples]
        self.assertEqual(frags, [0, 0, 0, 10, 11, 12, 13])  # values 0, 16, 32, then 65530-65533

    def test_the_diagnostic_reports_steps_without_gating(self):
        frames = recording(seconds=1, counter16=True, first=1)
        frames[5] = (frames[5][0], packet(body(rows_for(5)), seq=2, frag=7))  # 5 -> 39 -> 7
        counts, times, _, _ = self.admit(frames, counter_policy=True)
        self.assertEqual(len(times), 10)
        self.assertEqual((counts["counter_step_1"], counts["counter_step_other"],
                          counts["counter_step_17_to_4096"]), (7, 1, 1))

    def test_a_more_fragments_frame_quarantines_the_whole_file(self):
        frames = recording(seconds=2, counter16=True)
        frames.append((frames[-1][0] + 1, packet(body(rows_for(99)), seq=500, frag=1, fc=0x04e0)))
        counts, times, _, _ = self.admit(frames, counter_policy=True)
        self.assertEqual(counts["quarantined_file_has_more_fragments"], 18)
        self.assertEqual(counts["rejected_mac_guard_flags"], 1)
        self.assertEqual(len(times), 2)  # fragment-zero frames keep the strict handling

    def test_a_repeated_16_bit_value_within_1_s_is_quarantined(self):
        base = 10 * NS
        frames = [
            (base, packet(body(rows_for(1)), seq=9, frag=3)),
            (base + NS // 2, packet(body(rows_for(2)), seq=9, frag=3)),       # repeats 0x0093
            (base + NS // 2 + 1, packet(body(rows_for(3)), seq=9, frag=4)),   # next value: admitted
            (base + 3 * NS, packet(body(rows_for(4)), seq=9, frag=3)),        # 2.5 s later: admitted
            (base + 3 * NS + 1, packet(body(rows_for(5)), seq=10, frag=0)),   # fragment zero: v1 rules
        ]
        counts, times, _, _ = self.admit(frames, counter_policy=True)
        self.assertEqual(counts["quarantined_seqctl_duplicate"], 2)
        self.assertEqual(times, [base + NS // 2 + 1, base + 3 * NS, base + 3 * NS + 1])

    def test_policy_refusals_match_adr_427(self):
        base = 10 * NS
        frames = [
            (base, packet(body(rows_for(1)), seq=1, frag=3)),                          # admitted
            (base + 1, packet(body(rows_for(2)), seq=2, frag=3, fc=0x00d0)),            # Action, not No Ack
            (base + 2, packet(body(rows_for(3)), seq=3, frag=3, fcs=False)),            # no FCS
            (base + 3, packet(body(rows_for(4)), seq=4, frag=3, corrupt_fcs=True)),     # bad FCS
            (base + 4, packet(body(rows_for(5)) + b"\x00", seq=5, frag=3)),            # extended body
            (base + 5, packet(body(rows_for(6))[:-1], seq=6, frag=3)),                  # truncated body
        ]
        counts, times, _, _ = self.admit(frames, counter_policy=True)
        self.assertEqual(times, [base])
        expected = {"quarantined_policy_not_action_no_ack": 1, "quarantined_policy_no_fcs": 1,
                    "rejected_fcs_mismatch": 1, "rejected_body_length": 2,
                    "admitted_under_fragment_policy": 1}
        for key, value in expected.items():
            self.assertEqual(counts[key], value, key)

    def test_the_two_policies_cannot_be_combined(self):
        with self.assertRaises(motion.DecodeError):
            self.admit(recording(seconds=1), fragment_policy=True, counter_policy=True)

    def test_protocols_name_their_policies(self):
        self.assertEqual(motion.POLICIES, {"public-bfi-motion-1": None,
                                           "public-bfi-motion-2": motion.FRAGMENT_POLICY,
                                           "public-bfi-motion-3": motion.COUNTER_POLICY})
        self.assertIn(motion.COUNTER_POLICY, " ".join(motion.limitations("public-bfi-motion-3")))


class IntervalTests(unittest.TestCase):
    def eligible(self, times):
        return [i["eligible"] for i in motion.observed_intervals(times)]

    def test_twenty_even_reports_fill_one_interval(self):
        self.assertEqual(self.eligible([j * NS // 4 for j in range(20)]), [True])
        self.assertEqual(self.eligible([j * NS // 4 for j in range(19)]), [False])

    def test_gap_bounds_are_inclusive(self):
        base = [j * NS // 5 for j in range(25)]  # 0.0 .. 4.8 s
        self.assertEqual(self.eligible(base), [True])
        gap = [t for t in base if not 2 * NS < t < 3 * NS]  # exactly 1 s gap
        self.assertEqual(self.eligible(gap), [True])
        wider = [t + (1 if t >= 3 * NS else 0) for t in gap]
        self.assertEqual(self.eligible(wider), [False])

    def test_leading_and_trailing_edges(self):
        first = [j * NS // 5 for j in range(25)]
        on_time = first + [5 * NS + NS + j * NS // 10 for j in range(36)]  # 6.0 .. 9.5 s
        self.assertEqual(self.eligible(on_time), [True, True])
        late_start = [t + (1 if t >= 6 * NS else 0) for t in on_time]
        self.assertEqual(self.eligible(late_start), [True, False])
        early_end = first + [5 * NS + j * NS // 10 for j in range(21)]  # last at 7.0 s
        self.assertEqual(self.eligible(early_end), [True, False])
        exact_end = first + [5 * NS + j * NS // 10 for j in range(41)]  # last at 9.0 s
        self.assertEqual(self.eligible(exact_end), [True, True])

    def test_features_use_only_pairs_inside_eligible_intervals(self):
        times = [j * NS // 4 for j in range(20)] + [5 * NS + j * NS // 4 for j in range(20)]
        changes = [None] + [1.0] * 19 + [99.0] + [2.0] * 19
        intervals = motion.observed_intervals(times)
        self.assertEqual(motion.interval_features(intervals, changes), [(1.0, 20), (2.0, 20)])

    def test_span_is_bounded(self):
        with self.assertRaises(motion.DecodeError):
            motion.observed_intervals([0, motion.MAX_SPAN_NS + 1])


class ScoringTests(unittest.TestCase):
    def test_threshold_direction_and_ties(self):
        fit = motion.fit_threshold([0.1, 0.2, 0.8, 0.9], [False, False, True, True])
        self.assertEqual((fit["threshold"], fit["direction"], fit["train_balanced_accuracy"]), (0.5, "greater", 1.0))
        fit = motion.fit_threshold([0.1, 0.2, 0.8, 0.9], [True, True, False, False])
        self.assertEqual((fit["threshold"], fit["direction"]), (0.5, "less"))
        fit = motion.fit_threshold([0.3, 0.3], [True, False])
        self.assertEqual((fit["threshold"], fit["direction"], fit["train_balanced_accuracy"]), (-0.7, "greater", 0.5))

    def test_confusion_and_balanced_accuracy(self):
        result = motion.confusion([True, False, True, True], [True, True, False, False])
        self.assertEqual((result["tp"], result["fn"], result["tn"], result["fp"]), (1, 1, 0, 2))
        self.assertEqual(result["balanced_accuracy"], 0.25)

    def test_admission_requires_two_recordings_everywhere_and_an_oracle(self):
        records = []
        for group in motion.GROUPS:
            for activity in motion.ACTIVITIES:
                for participant in ("P1", "P2"):
                    records.append({"group": group, "activity": activity, "participant": participant,
                                    "status": "ADMITTED", "eligible_intervals": 1, "path": f"{group}{activity}{participant}",
                                    "oracle": {"compared": 4, "status": "MATCH"}})
        groups = motion.summarize(records, set(motion.GROUPS))
        self.assertTrue(motion.admission(groups, records)["admitted"])
        records[-1]["eligible_intervals"] = 0
        gate = motion.admission(motion.summarize(records, set(motion.GROUPS)), records)
        self.assertFalse(gate["admitted"])
        self.assertEqual(gate["shortfalls"], [{"group": motion.GROUPS[-1], "activity": "T", "recordings": 1}])
        records[-1]["eligible_intervals"] = 1
        records[0]["oracle"] = {"compared": 0, "status": "FAILED", "reason": "oracle_angle_mismatch"}
        gate = motion.admission(motion.summarize(records, set(motion.GROUPS)), records)
        self.assertEqual(gate["reasons"], ["decoder_oracle_failed"])
        records[0]["oracle"] = {"compared": 4, "status": "MATCH"}
        gate = motion.admission(motion.summarize(records, set(motion.GROUPS[:2])), records)
        self.assertIn("group_not_downloaded:heldout_same_device", gate["reasons"])

    def test_score_fits_on_train_only(self):
        records, features = [], []
        for group, offset in (("train", 0.0), ("validation", 10.0)):
            for activity in motion.ACTIVITIES:
                for participant in ("P1", "P2"):
                    records.append({"group": group, "activity": activity, "participant": participant,
                                    "path": f"{group}/{activity}_{participant}"})
                    value = (1.0 if activity in motion.MOVING else 0.1) + offset
                    features.append([(value, 40), (value, 41)])
        scores = motion.score(features, records)
        self.assertEqual(scores["bfi_motion"]["train_balanced_accuracy"], 1.0)
        self.assertAlmostEqual(scores["bfi_motion"]["threshold"], 0.55)
        self.assertEqual(scores["bfi_motion"]["validation_intervals"]["balanced_accuracy"], 0.5)
        self.assertEqual(scores["coverage"]["validation"], {"eligible_intervals": 80, "recordings": 40})


@unittest.skipUnless(WIRESHARK, "Wireshark 4.2.2 tshark/editcap not installed")
class WiresharkTests(unittest.TestCase):
    def test_editcap_conversion_is_lossless(self):
        packets = recording(seconds=2)
        raw = pcapng(packets)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "x.pcapng"
            path.write_bytes(raw)
            _, source = motion.pcapng_packets(raw)
            derived, receipt = motion.lossless_conversion(path, source, shutil.which("editcap"), directory)
            self.assertEqual(derived, source)
            self.assertTrue(receipt["bytes_equal"])
            self.assertFalse((Path(directory) / "derivative.pcap").exists())

    def test_oracle_matches_and_detects_a_wrong_angle(self):
        raw = [(t, len(p), len(p), p) for t, p in recording(seconds=1)]
        _, _, _, samples = motion.admit_packets(raw)
        tshark = shutil.which("tshark")
        self.assertEqual(motion.oracle_compare(tshark, samples), 4)
        packet_bytes, report = samples[0]
        wrong = dict(report, quantized=((1, 0, 0, 0),) + report["quantized"][1:])
        with self.assertRaises(motion.DecodeError) as caught:
            motion.oracle_compare(tshark, [(packet_bytes, wrong)])
        self.assertEqual(caught.exception.reason, "oracle_angle_mismatch")
        wrong = dict(report, avg_snr_code=report["avg_snr_code"] + 1)
        with self.assertRaises(motion.DecodeError) as caught:
            motion.oracle_compare(tshark, [(packet_bytes, wrong)])
        self.assertEqual(caught.exception.reason, "oracle_snr_mismatch")

    def test_policy_frames_match_tshark(self):
        raw = [(t, len(p), len(p), p) for t, p in recording(seconds=1, counter_bits=True)]
        _, _, _, samples = motion.admit_packets(raw, fragment_policy=True)
        self.assertEqual(motion.oracle_compare(shutil.which("tshark"), samples), 5)

    def test_census_file_with_the_fragment_policy(self):
        raw = pcapng(recording(seconds=12, moving=True, counter_bits=True))
        import hashlib
        entry = {"path": "HAR-1/BFI/M1/A_1_M1_P1.pcapng", "split": "train", "activity_code": "A",
                 "participant_code": "P1", "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
        tools = {"tshark": shutil.which("tshark"), "editcap": shutil.which("editcap")}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "file.pcapng"
            path.write_bytes(raw)
            strict, _ = motion.census_file(entry, path, tools, directory)
            policy, _ = motion.census_file(entry, path, tools, directory, fragment_policy=True)
        self.assertEqual(strict["eligible_intervals"], 0)
        self.assertEqual(policy["eligible_intervals"], 2)
        self.assertEqual(policy["oracle"], {"compared": 8, "status": "MATCH", "policy_compared": 4})

    def test_counter_policy_frames_match_tshark(self):
        raw = [(t, len(p), len(p), p) for t, p in recording(seconds=4, counter16=True, first=7)]
        _, _, _, samples = motion.admit_packets(raw, counter_policy=True)
        self.assertEqual(motion.oracle_compare(shutil.which("tshark"), samples), 6)  # 16, 32 + 4 policy

    def test_census_file_with_the_counter_policy(self):
        raw = pcapng(recording(seconds=12, moving=True, counter16=True))
        import hashlib
        entry = {"path": "HAR-1/BFI/M1/A_1_M1_P1.pcapng", "split": "train", "activity_code": "A",
                 "participant_code": "P1", "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
        tools = {"tshark": shutil.which("tshark"), "editcap": shutil.which("editcap")}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "file.pcapng"
            path.write_bytes(raw)
            siblings, _ = motion.census_file(entry, path, tools, directory, fragment_policy=True)
            counter, _ = motion.census_file(entry, path, tools, directory, counter_policy=True)
        self.assertEqual(siblings["eligible_intervals"], 0)
        self.assertEqual(counter["eligible_intervals"], 2)
        self.assertEqual(counter["oracle"], {"compared": 8, "status": "MATCH", "policy_compared": 4})

    def test_census_file_end_to_end(self):
        raw = pcapng(recording(seconds=12, moving=True))
        import hashlib
        entry = {"path": "HAR-1/BFI/M1/A_1_M1_P1.pcapng", "split": "train", "activity_code": "A",
                 "participant_code": "P1", "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
        tools = {"tshark": shutil.which("tshark"), "editcap": shutil.which("editcap")}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "file.pcapng"
            path.write_bytes(raw)
            record, features = motion.census_file(entry, path, tools, directory)
            self.assertEqual(record["status"], "ADMITTED")
            self.assertEqual(record["oracle"], {"compared": 4, "status": "MATCH"})
            self.assertEqual(record["intervals"], 3)
            self.assertEqual(record["eligible_intervals"], 2)  # the third holds only the final 2 s
            self.assertEqual(len(features), 2)
            self.assertGreater(features[0][0], 0.0)
            self.assertEqual((record["gaps_over_1s"], record["max_gap_seconds"]), (0, 0.1))
            entry["sha256"] = "0" * 64
            record, _ = motion.census_file(entry, path, tools, directory)
            self.assertEqual((record["status"], record["reason"]), ("REJECTED", "integrity_mismatch"))

    def test_main_writes_census_and_does_not_score_unadmitted_data(self):
        import json
        manifest = (Path(motion.HERE) / "manifests/csi-bfi-har-c3013b3-subset.json").read_bytes()
        entries = json.loads(manifest)["files"][:2]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "data"
            root.mkdir()
            (root / "source-manifest.json").write_bytes(manifest)
            receipt = {"status": "COMPLETED", "manifest_sha256": motion.download.MANIFEST_SHA256,
                       "files": [{"path": e["path"], "status": "DOWNLOADED"} for e in entries]}
            (root / "download-receipt.json").write_text(json.dumps(receipt))
            for entry in entries:
                target = root / entry["path"]
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(pcapng(recording(seconds=1)))
            report_md, report_json = Path(directory) / "r.md", Path(directory) / "r.json"
            code = motion.main(["--data", str(root), "--report", str(report_md), "--json", str(report_json),
                                "--commit", "test"])
            self.assertEqual(code, 0)
            report = json.loads(report_json.read_text())
            self.assertFalse(report["admission"]["admitted"])
            self.assertIsNone(report["scores"])
            self.assertEqual(report["census"]["groups"]["train"]["rejected_files"], {"integrity_mismatch": 2})
            self.assertIn("group_not_downloaded:validation", report["admission"]["reasons"])
            self.assertIn("NOT ADMITTED", report_md.read_text())
            self.assertNotIn(STA.hex(), report_json.read_text())


if __name__ == "__main__":
    unittest.main()

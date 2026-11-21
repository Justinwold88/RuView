#!/usr/bin/env python3
"""Public BFI motion test, protocols public-bfi-motion-1 to -3 (PUBLIC-BFI-MOTION*.md).

Offline and bounded. Reads a private download made by download_public_bfi.py and
runs the ADR-365 quality census on all four partition/view groups. Checks the
body decoder against tshark 4.2.2. Only if the benchmark is admitted, fits a
one-feature threshold on train and scores it on validation. Held-out groups are
inspected for quality only and are never scored.

The report holds aggregate numbers only: no addresses, angles or packet bytes.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import os
import platform
import re
import shutil
import statistics
import struct
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "bfi"))
import bfi_decode as decoder  # noqa: E402
import compare_tshark  # noqa: E402
import download_public_bfi as download  # noqa: E402
import public_bfi_decode  # noqa: E402

PROTOCOL = "public-bfi-motion-1"
PROTOCOLS = ("public-bfi-motion-1", "public-bfi-motion-2", "public-bfi-motion-3")
# ADR-427: the only route by which a nonzero fragment number is admitted in protocol 2.
FRAGMENT_POLICY = "csi-bfi-har-complete-mpdu-v1"
# ADR-428: the route in protocol 3, which reads all 16 Sequence Control bits as one counter.
COUNTER_POLICY = "csi-bfi-har-seqctl-counter-v1"
POLICIES = {"public-bfi-motion-1": None, "public-bfi-motion-2": FRAGMENT_POLICY,
            "public-bfi-motion-3": COUNTER_POLICY}
SIBLING_WINDOW_NS = 1_000_000_000
DUPLICATE_WINDOW_NS = 1_000_000_000
WIRESHARK_VERSION = "4.2.2"
MAX_FILE = download.MAX_FILE
MAX_BLOCK = 1024 * 1024
MAX_PACKETS = 500_000
MAX_SNAPLEN = 262_144
MAX_SPAN_NS = 24 * 3600 * 1_000_000_000
NS = 1_000_000_000
INTERVAL_NS = 5 * NS
MAX_GAP_NS = NS
MIN_REPORTS = 20
MIN_RECORDINGS = 2
ORACLE_REPORTS = 4
RETRY_WINDOW_NS = NS
RETRY_CACHE = 4096
ACTIVITIES = tuple("ABCDEFGHIJKLMNOPQRST")
MOVING = frozenset("ABCDGKLOQ")
GROUPS = download.SPLITS
FIT_GROUP, SCORE_GROUP = "train", "validation"
SHB, IDB, OPB, SPB, EPB = 0x0A0D0D0A, 1, 2, 3, 6
FC_GUARDS = 0xC700  # ToDS, FromDS, More Fragments, Protected, Order
FC_MORE_FRAGMENTS = 0x0400
# Protocol 3 diagnostic: forward steps of the 16-bit Sequence Control value between
# consecutive candidates of the chosen direction. Reported only; never a gate.
COUNTER_STEPS = (("counter_step_1", 1, 1), ("counter_step_2_to_16", 2, 16),
                 ("counter_step_17_to_4096", 17, 4096))
FC_RETRY = 0x0800
ANGLE_PATTERN = re.compile(r"-?\d+, φ11:(\d+), φ21:(\d+), ψ21:(\d+), ψ31:(\d+)")
CONTROL_FIELDS = (("ncindex", lambda c: c["nc"] - 1), ("nrindex", lambda c: c["nr"] - 1),
                  ("chanwidth", lambda c: {20: 0, 40: 1, 80: 2, 160: 3}[c["width_mhz"]]),
                  ("grouping", lambda c: c["grouping_index"]), ("codebookinfo", lambda c: c["codebook"]),
                  ("feedbacktype", lambda c: c["feedback_type"]),
                  ("remainingfeedbackseg", lambda c: c["remaining_segments"]),
                  ("firstfeedbackseg", lambda c: c["first_segment"]),
                  ("reserved", lambda c: c["reserved"]),
                  ("sounding_dialog_token_nbr", lambda c: c["sounding_token"]))
BODY_ERRORS = {"expected exactly 1003 bytes of action body": "rejected_body_length",
               "unsupported VHT MU tuple, reserved bits or segmentation": "rejected_unsupported_tuple"}
require = decoder.require
DecodeError = decoder.DecodeError
assert MOVING < frozenset(ACTIVITIES)


# ---------------------------------------------------------------- file layer

def _options(data, endian):
    found = collections.defaultdict(list)
    offset = 0
    while offset + 4 <= len(data):
        code, size = struct.unpack_from(endian + "HH", data, offset)
        offset += 4
        if code == 0:
            break
        require(offset + size <= len(data), "pcapng_option_truncated")
        found[code].append(data[offset:offset + size])
        offset += (size + 3) & ~3
    return found


def _ns_per_tick(options, endian):
    """if_tsresol must give a whole number of nanoseconds per tick; no offset."""
    values = options.get(9, [])
    require(len(values) <= 1, "pcapng_duplicate_timestamp_resolution")
    exponent = 6
    if values:
        require(len(values[0]) == 1, "pcapng_invalid_timestamp_resolution")
        exponent = values[0][0]
        require(not exponent & 0x80 and exponent <= 9, "pcapng_timestamp_resolution_not_whole_ns")
    for value in options.get(14, []):
        require(len(value) == 8 and struct.unpack(endian + "q", value)[0] == 0,
                "pcapng_timestamp_offset_present")
    return exponent, 10 ** (9 - exponent)


def pcapng_packets(raw):
    """Narrow PCAPNG reader: (inventory, [(ns, captured, original, bytes)]).

    One section, one radiotap interface, an exact-nanosecond clock and Enhanced
    Packet Blocks only. Anything else fails closed with a reason code.
    """
    require(len(raw) <= MAX_FILE, "file_limit_exceeded")
    require(len(raw) >= 28 and struct.unpack_from("<I", raw)[0] == SHB, "pcapng_section_header_missing")
    endian = {b"\x4d\x3c\x2b\x1a": "<", b"\x1a\x2b\x3c\x4d": ">"}.get(raw[8:12])
    require(endian is not None, "pcapng_unknown_byte_order")
    offset, sections, interface, packets = 0, 0, None, []
    other_blocks, packet_options = collections.Counter(), 0
    while offset < len(raw):
        require(offset + 12 <= len(raw), "pcapng_block_truncated")
        block_type, length = struct.unpack_from(endian + "II", raw, offset)
        require(length % 4 == 0 and 12 <= length <= MAX_BLOCK and offset + length <= len(raw),
                "pcapng_invalid_block_length")
        require(struct.unpack_from(endian + "I", raw, offset + length - 4)[0] == length,
                "pcapng_trailing_length_mismatch")
        body = raw[offset + 8:offset + length - 4]
        if block_type == SHB:
            sections += 1
            require(sections == 1, "pcapng_multiple_sections")
            require(len(body) >= 16 and body[:4] == raw[8:12], "pcapng_invalid_section_header")
            require(struct.unpack_from(endian + "H", body, 4)[0] == 1, "pcapng_unsupported_version")
        elif block_type == IDB:
            require(sections == 1 and interface is None, "pcapng_multiple_interfaces")
            require(len(body) >= 8, "pcapng_interface_truncated")
            linktype, _, snaplen = struct.unpack_from(endian + "HHI", body)
            require(linktype == 127, "pcapng_linktype_not_radiotap")
            options = _options(body[8:], endian)
            exponent, per_tick = _ns_per_tick(options, endian)
            fcslen = options.get(13, [])
            interface = {"linktype": linktype, "snaplen": snaplen, "tsresol_exponent": exponent,
                         "ns_per_tick": per_tick,
                         "fcslen_option": fcslen[0][0] if fcslen and len(fcslen[0]) == 1 else None}
        elif block_type == EPB:
            require(interface is not None, "pcapng_packet_before_interface")
            require(len(body) >= 20, "pcapng_packet_block_truncated")
            iface, high, low, captured, original = struct.unpack_from(endian + "IIIII", body)
            require(iface == 0, "pcapng_unknown_interface")
            require(captured <= original and (interface["snaplen"] == 0 or captured <= interface["snaplen"]),
                    "pcapng_invalid_packet_lengths")
            padded = (captured + 3) & ~3
            require(20 + padded <= len(body), "pcapng_packet_data_truncated")
            packet_options += 20 + padded < len(body)
            require(len(packets) < MAX_PACKETS, "packet_limit_exceeded")
            packets.append((((high << 32) | low) * interface["ns_per_tick"], captured, original,
                            body[20:20 + captured]))
        elif block_type in (SPB, OPB):
            raise DecodeError("pcapng_packet_block_without_clock")
        else:
            other_blocks[f"0x{block_type:08x}"] += 1
        offset += length
    require(sections == 1 and interface is not None, "pcapng_missing_section_or_interface")
    inventory = dict(byte_order="little" if endian == "<" else "big", packets=len(packets),
                     packet_blocks_with_options=packet_options, other_blocks=dict(other_blocks),
                     **{k: v for k, v in interface.items() if k != "ns_per_tick"})
    return inventory, packets


def nsec_pcap_packets(raw):
    """Reader for the editcap nsecpcap derivative only: [(ns, captured, original, bytes)]."""
    require(len(raw) >= 24, "derivative_header_truncated")
    endian = {b"\x4d\x3c\xb2\xa1": "<", b"\xa1\xb2\x3c\x4d": ">"}.get(raw[:4])
    require(endian is not None, "derivative_not_nsec_pcap")
    major, minor, _, _, snaplen, linktype = struct.unpack_from(endian + "HHIIII", raw, 4)
    require((major, minor) == (2, 4), "derivative_unsupported_version")
    require(linktype == 127, "derivative_linktype_not_radiotap")
    require(0 < snaplen <= MAX_SNAPLEN, "derivative_invalid_snaplen")
    offset, packets = 24, []
    while offset < len(raw):
        require(offset + 16 <= len(raw), "derivative_record_truncated")
        seconds, fraction, captured, original = struct.unpack_from(endian + "IIII", raw, offset)
        require(fraction < NS, "derivative_invalid_timestamp")
        require(captured <= original and captured <= snaplen, "derivative_invalid_lengths")
        require(offset + 16 + captured <= len(raw), "derivative_data_truncated")
        require(len(packets) < MAX_PACKETS, "packet_limit_exceeded")
        packets.append((seconds * NS + fraction, captured, original, raw[offset + 16:offset + 16 + captured]))
        offset += 16 + captured
    return packets


def verify_lossless(source, derived):
    require(len(source) == len(derived), "conversion_packet_count_mismatch")
    for a, b in zip(source, derived):
        require(a[:3] == b[:3], "conversion_timestamp_or_length_mismatch")
        require(a[3] == b[3], "conversion_packet_bytes_mismatch")
    return True


def lossless_conversion(path, source_packets, editcap, workdir):
    """editcap -F nsecpcap, then prove equality with the independent inventory."""
    derivative = Path(workdir) / "derivative.pcap"
    try:
        compare_tshark.run_bounded([editcap, "-F", "nsecpcap", str(path), str(derivative)], timeout=120)
        require(derivative.is_file() and derivative.stat().st_size <= MAX_FILE, "derivative_missing_or_too_large")
        derived_raw = derivative.read_bytes()
    finally:
        derivative.unlink(missing_ok=True)
    derived = nsec_pcap_packets(derived_raw)
    verify_lossless(source_packets, derived)
    return derived, {"argv": ["editcap", "-F", "nsecpcap", "<source>", "<derivative>"],
                     "derivative_sha256": hashlib.sha256(derived_raw).hexdigest(),
                     "packet_count": len(derived), "timestamps_equal": True,
                     "lengths_equal": True, "bytes_equal": True}


# -------------------------------------------------------------- packet layer

def radians(quantized):
    """Quantization bin centres, matching public_bfi_decode (both denominators are 512)."""
    return (2.0 * np.asarray(quantized, dtype=np.float64) + 1.0) * math.pi / 512.0


def angle_change(previous, current):
    """Mean absolute change over 234 tones x (phi11, phi21 circular; psi21, psi31 plain)."""
    delta = current - previous
    delta[:, :2] = (delta[:, :2] + math.pi) % (2 * math.pi) - math.pi
    return float(np.abs(delta).mean())


def fcs_verified(data, frame):
    """True if strip_radiotap removed, and therefore CRC-checked, exactly four FCS bytes."""
    return len(data) - struct.unpack_from("<H", data, 2)[0] - len(frame) == 4


def fragment_siblings(candidates):
    """Positions of policy candidates sharing a sequence number with a different fragment number within 1 s."""
    groups = collections.defaultdict(list)
    for position, (_, timestamp, _, frame, pair, fragment) in enumerate(candidates):
        groups[(pair, int.from_bytes(frame[22:24], "little") >> 4)].append((timestamp, fragment, position))
    siblings = set()
    for members in groups.values():
        for timestamp, fragment, position in members:
            if fragment and any(other != fragment and abs(t - timestamp) <= SIBLING_WINDOW_NS
                                for t, other, _ in members):
                siblings.add(position)
    return siblings


def seqctl_duplicates(candidates):
    """Positions of policy candidates sharing all 16 Sequence Control bits with another
    candidate from the same pair within 1 s (ADR-428)."""
    groups = collections.defaultdict(list)
    for position, (_, timestamp, _, frame, pair, _) in enumerate(candidates):
        groups[(pair, frame[22:24])].append((timestamp, position))
    duplicates = set()
    for members in groups.values():
        for timestamp, position in members:
            if candidates[position][5] and any(other != position and abs(t - timestamp) <= DUPLICATE_WINDOW_NS
                                               for t, other in members):
                duplicates.add(position)
    return duplicates


def counter_steps(candidates, pair):
    """Diagnostic histogram of 16-bit Sequence Control steps between consecutive candidates of `pair`."""
    steps = collections.Counter()
    previous = None
    for _, _, _, frame, candidate_pair, _ in candidates:
        if candidate_pair != pair:
            continue
        value = int.from_bytes(frame[22:24], "little")
        if previous is not None:
            step = (value - previous) % 65536
            name = next((n for n, low, high in COUNTER_STEPS if low <= step <= high), "counter_step_other")
            steps[name] += 1
        previous = value
    return steps


def admit_packets(packets, oracle_reports=ORACLE_REPORTS, fragment_policy=False, counter_policy=False):
    """Apply the nine frozen packet steps. Returns (counts, times, changes, oracle samples).

    changes[j] is the angle change from admitted report j-1 to j (changes[0] is None).
    With fragment_policy (protocol 2, ADR-427), step 5 admits nonzero fragment numbers only
    as complete, CRC-checked Action No Ack frames without fragment siblings. With
    counter_policy (protocol 3, ADR-428), it admits them as complete, CRC-checked Action
    No Ack frames when no feedback frame in the file sets More Fragments and no other
    candidate from the same pair repeats all 16 Sequence Control bits within 1 s. With
    either policy the oracle samples hold up to `oracle_reports` fragment-zero reports and
    as many policy reports.
    """
    require(not (fragment_policy and counter_policy), "conflicting_fragment_policies")
    policy = fragment_policy or counter_policy
    counts = collections.Counter()
    candidates = []
    more_fragments = False
    for index, (timestamp, captured, original, data) in enumerate(packets):
        if captured != original:
            counts["rejected_snaplen_truncated"] += 1
            continue
        try:
            frame = decoder.strip_radiotap(data)
        except DecodeError as error:
            counts["rejected_" + error.reason] += 1
            continue
        fc = int.from_bytes(frame[:2], "little") if len(frame) >= 2 else None
        if fc is None or fc & 0xF or (fc >> 4) & 0xF not in (13, 14):
            counts["other_traffic"] += 1
            continue
        if len(frame) < 26:
            counts["rejected_short_action_frame"] += 1
            continue
        if frame[24:26] != b"\x15\x00":
            counts["other_traffic"] += 1
            continue
        if fc & FC_GUARDS:
            if fc & FC_MORE_FRAGMENTS:
                more_fragments = True
            counts["rejected_mac_guard_flags"] += 1
            continue
        fragment = frame[22] & 0x0F
        if policy:
            counts[f"fragment_number_{fragment:02d}"] += 1
        if fragment:
            if not policy:
                counts["quarantined_nonzero_fragment"] += 1
                continue
            if (fc >> 4) & 0xF != 14:
                counts["quarantined_policy_not_action_no_ack"] += 1
                continue
            if not fcs_verified(data, frame):
                counts["quarantined_policy_no_fcs"] += 1
                continue
        candidates.append((index, timestamp, fc, frame, frame[10:16] + frame[4:10], fragment))
    if fragment_policy:
        siblings = fragment_siblings(candidates)
        counts["quarantined_fragment_sibling"] += len(siblings)
        candidates = [c for position, c in enumerate(candidates) if position not in siblings]
    unfiltered = list(candidates)
    if counter_policy:
        if more_fragments:
            refused = {position for position, c in enumerate(candidates) if c[5]}
            counts["quarantined_file_has_more_fragments"] += len(refused)
        else:
            refused = seqctl_duplicates(candidates)
            counts["quarantined_seqctl_duplicate"] += len(refused)
        candidates = [c for position, c in enumerate(candidates) if position not in refused]
    pairs, first_seen = collections.Counter(), {}
    for position, candidate in enumerate(candidates):
        pairs[candidate[4]] += 1
        first_seen.setdefault(candidate[4], position)
    chosen = max(pairs, key=lambda pair: (pairs[pair], -first_seen[pair])) if pairs else None
    counts["candidate_pairs"] = len(pairs)
    if counter_policy and chosen is not None:
        counts.update(counter_steps(unfiltered, chosen))
    retry_seen, payloads = collections.OrderedDict(), set()
    times, changes, samples, policy_samples = [], [], [], []
    previous_angles = None
    for index, timestamp, fc, frame, pair, fragment in candidates:
        if pair != chosen:
            counts["rejected_other_direction"] += 1
            continue
        body = frame[24:]
        try:
            report = public_bfi_decode.decode_body(body)
        except ValueError as error:
            counts[BODY_ERRORS.get(str(error), "rejected_unsupported_format")] += 1
            continue
        digest = hashlib.sha256(body).digest()
        key = (frame[22:24], digest)
        prior = retry_seen.get(key)
        if fc & FC_RETRY and prior is not None and timestamp - prior < RETRY_WINDOW_NS:
            counts["dropped_retry_duplicate"] += 1
            continue
        if times and timestamp <= times[-1]:
            counts["rejected_nonincreasing_timestamp"] += 1
            continue
        if digest in payloads:
            counts["duplicate_payload_admitted"] += 1
        payloads.add(digest)
        retry_seen[key] = timestamp
        retry_seen.move_to_end(key)
        if len(retry_seen) > RETRY_CACHE:
            retry_seen.popitem(last=False)
        angles = radians(report["quantized"])
        changes.append(None if previous_angles is None else angle_change(previous_angles, angles))
        previous_angles = angles
        times.append(timestamp)
        if fragment:
            counts["admitted_under_fragment_policy"] += 1
            if len(policy_samples) < oracle_reports:
                policy_samples.append((packets[index][3], report))
        elif len(samples) < oracle_reports:
            samples.append((packets[index][3], report))
    counts["admitted_reports"] = len(times)
    return counts, times, changes, samples + policy_samples


# ------------------------------------------------------------ interval layer

def observed_intervals(times):
    """Frozen tiling: [t0+5k, t0+5(k+1)) up to the interval holding the last report."""
    if not times:
        return []
    require(times[-1] - times[0] <= MAX_SPAN_NS, "observed_span_exceeds_limit")
    t0 = times[0]
    members = [[] for _ in range((times[-1] - t0) // INTERVAL_NS + 1)]
    for index, timestamp in enumerate(times):
        members[(timestamp - t0) // INTERVAL_NS].append(index)
    intervals = []
    for k, indices in enumerate(members):
        start, end = t0 + k * INTERVAL_NS, t0 + (k + 1) * INTERVAL_NS
        stamps = [times[i] for i in indices]
        eligible = (len(stamps) >= MIN_REPORTS and stamps[0] - start <= MAX_GAP_NS
                    and end - stamps[-1] <= MAX_GAP_NS
                    and all(b - a <= MAX_GAP_NS for a, b in zip(stamps, stamps[1:])))
        intervals.append({"k": k, "indices": indices, "eligible": eligible})
    return intervals


def interval_features(intervals, changes):
    """(motion feature, timing feature) per eligible interval, in order."""
    out = []
    for interval in intervals:
        if not interval["eligible"]:
            continue
        indices = interval["indices"]
        pairs = [changes[i] for i in indices[1:]]
        out.append((statistics.median(pairs), len(indices)))
    return out


# --------------------------------------------------------------- oracle layer

def oracle_pcap(samples):
    out = [struct.pack("<IHHIIII", 0xa1b2c3d4, 2, 4, 0, 0, 65535, 127)]
    for number, (packet, _) in enumerate(samples, 1):
        require(len(packet) <= 65535, "oracle_packet_too_large")
        out.append(struct.pack("<IIII", number, 0, len(packet), len(packet)) + packet)
    return b"".join(out)


def oracle_values(pdml, samples):
    """Compare tshark PDML with decoded reports; returns matched report count."""
    require(len(pdml) <= compare_tshark.MAX_OUTPUT, "oracle_output_limit_exceeded")
    upper = pdml.upper()
    require(b"<!DOCTYPE" not in upper and b"<!ENTITY" not in upper, "unsafe_oracle_xml")
    try:
        root = ET.fromstring(pdml)
    except ET.ParseError as error:
        raise DecodeError("invalid_oracle_xml") from error
    packets = root.findall("packet")
    require(len(packets) == len(samples), "oracle_packet_count_mismatch")
    for element, (_, report) in zip(packets, samples):
        require(not any(x.get("name") == "_ws.malformed" for x in element.iter()), "oracle_malformed_packet")
        fields = collections.defaultdict(list)
        for field in element.iter("field"):
            fields[field.get("name", "")].append(field.get("show", ""))
        try:
            for name, expected in CONTROL_FIELDS:
                values = fields["wlan.vht.mimo_control." + name]
                require(len(values) == 1, "oracle_control_field_missing_or_ambiguous")
                require(int(values[0], 0) == expected(report["control"]), "oracle_control_mismatch")
            snr = fields["wlan.vht.compressed_beamforming_report.snr"]
            require(len(snr) == 1 and int(snr[0], 0) == report["avg_snr_code"], "oracle_snr_mismatch")
        except ValueError as error:
            if isinstance(error, DecodeError):
                raise
            raise DecodeError("invalid_oracle_numeric_field") from error
        angles = []
        for value in fields["wlan.vht.compressed_beamforming_report.scidx"]:
            match = ANGLE_PATTERN.fullmatch(value)
            require(match is not None, "unsupported_oracle_angle_display")
            angles.append(tuple(int(group) for group in match.groups()))
        require(len(angles) == public_bfi_decode.TONE_COUNT, "oracle_tone_count_mismatch")
        require(tuple(angles) == report["quantized"], "oracle_angle_mismatch")
    return len(samples)


def oracle_compare(tshark, samples):
    if not samples:
        return 0
    pdml = compare_tshark.run_bounded([tshark, "-n", "-r", "-", "-T", "pdml"], oracle_pcap(samples), timeout=120)
    return oracle_values(pdml, samples)


def tool_version(executable, product):
    line = compare_tshark.run_bounded([executable, "--version"]).decode("utf-8", "replace").splitlines()[0]
    require(re.search(rf"\b{product} \(Wireshark\) {re.escape(WIRESHARK_VERSION)}\b", line) is not None,
            f"unsupported_{product.lower()}_version")
    return line


# ---------------------------------------------------------------- census

def census_file(entry, path, tools, workdir, fragment_policy=False, counter_policy=False):
    """One file's census record and its private eligible-interval features."""
    record = {"path": entry["path"], "group": entry["split"], "activity": entry["activity_code"],
              "participant": entry["participant_code"], "status": "ADMITTED"}
    try:
        raw = download.read_bounded(path, MAX_FILE)
        record["bytes"] = len(raw)
        require(len(raw) == entry["bytes"] and hashlib.sha256(raw).hexdigest() == entry["sha256"],
                "integrity_mismatch")
        inventory, source = pcapng_packets(raw)
        del raw
        record["inventory"] = inventory
        packets, record["conversion"] = lossless_conversion(path, source, tools["editcap"], workdir)
        del source
        counts, times, changes, samples = admit_packets(packets, fragment_policy=fragment_policy,
                                                        counter_policy=counter_policy)
        del packets
        record["packets"] = dict(sorted(counts.items()))
        intervals = observed_intervals(times)
        features = interval_features(intervals, changes)
        record["observed_span_seconds"] = (times[-1] - times[0]) / NS if times else 0.0
        gaps = [b - a for a, b in zip(times, times[1:])]
        record["max_gap_seconds"] = max(gaps) / NS if gaps else None
        record["gaps_over_1s"] = sum(gap > MAX_GAP_NS for gap in gaps)
        record["intervals"] = len(intervals)
        record["eligible_intervals"] = len(features)
        record["eligible_seconds"] = len(features) * INTERVAL_NS / NS
        try:
            record["oracle"] = {"compared": oracle_compare(tools["tshark"], samples), "status": "MATCH"}
            if fragment_policy or counter_policy:
                record["oracle"]["policy_compared"] = sum(
                    decoder.strip_radiotap(packet)[22] & 0x0F != 0 for packet, _ in samples)
        except DecodeError as error:
            record["oracle"] = {"compared": 0, "status": "FAILED", "reason": error.reason}
        if features:
            record["median_motion"] = statistics.median(f for f, _ in features)
        return record, features
    except (DecodeError, download.DownloadError, OSError) as error:
        record["status"] = "REJECTED"
        record["reason"] = getattr(error, "reason", None) or getattr(error, "code", None) or type(error).__name__
        return record, []


def summarize(records, downloaded_groups):
    groups = {}
    for group in GROUPS:
        rows = [r for r in records if r["group"] == group]
        coverage = {a: sum(1 for r in rows if r["activity"] == a and r.get("eligible_intervals", 0) > 0)
                    for a in ACTIVITIES}
        packets = collections.Counter()
        for r in rows:
            packets.update(r.get("packets", {}))
        groups[group] = {
            "downloaded": group in downloaded_groups,
            "files": len(rows),
            "admitted_files": sum(r["status"] == "ADMITTED" for r in rows),
            "rejected_files": dict(collections.Counter(r["reason"] for r in rows if r["status"] == "REJECTED")),
            "packet_outcomes": dict(sorted(packets.items())),
            "files_with_eligible_intervals": sum(r.get("eligible_intervals", 0) > 0 for r in rows),
            "intervals": sum(r.get("intervals", 0) for r in rows),
            "eligible_intervals": sum(r.get("eligible_intervals", 0) for r in rows),
            "eligible_seconds": sum(r.get("eligible_seconds", 0.0) for r in rows),
            "observed_span_seconds": sum(r.get("observed_span_seconds", 0.0) for r in rows),
            "recordings_with_eligible_interval_by_activity": coverage,
        }
    return groups


def admission(groups, records):
    reasons, shortfalls = [], []
    for group in GROUPS:
        if not groups[group]["downloaded"]:
            reasons.append(f"group_not_downloaded:{group}")
        for activity, recordings in groups[group]["recordings_with_eligible_interval_by_activity"].items():
            if recordings < MIN_RECORDINGS:
                shortfalls.append({"group": group, "activity": activity, "recordings": recordings})
    if shortfalls:
        reasons.append("activity_coverage_below_two_recordings")
    oracle = [r["oracle"] for r in records if "oracle" in r]
    compared = sum(o["compared"] for o in oracle)
    failures = collections.Counter(o["reason"] for o in oracle if o["status"] != "MATCH")
    if failures or compared == 0:
        reasons.append("decoder_oracle_failed" if failures else "decoder_oracle_compared_nothing")
    return {"admitted": not reasons, "reasons": reasons, "shortfalls": shortfalls,
            "oracle": {"reports_compared": compared, "files_compared": sum(o["compared"] > 0 for o in oracle),
                       "failures": dict(failures), "frequency_axis_validated": False}}


# ---------------------------------------------------------------- scoring

def confusion(predicted, moving):
    predicted, moving = np.asarray(predicted, bool), np.asarray(moving, bool)
    tp, fn = int(np.sum(predicted & moving)), int(np.sum(~predicted & moving))
    tn, fp = int(np.sum(~predicted & ~moving)), int(np.sum(predicted & ~moving))
    require(tp + fn > 0 and tn + fp > 0, "both_groups_required")
    return {"tp": tp, "fn": fn, "tn": tn, "fp": fp,
            "balanced_accuracy": 0.5 * (tp / (tp + fn) + tn / (tn + fp))}


def predict(values, threshold, direction):
    values = np.asarray(values, dtype=np.float64)
    return values > threshold if direction == "greater" else values < threshold


def fit_threshold(values, moving):
    """Highest train balanced accuracy; ties: lower threshold, then 'greater'."""
    distinct = sorted({float(v) for v in values})
    require(distinct, "no_training_values")
    candidates = [distinct[0] - 1.0] + [(a + b) / 2 for a, b in zip(distinct, distinct[1:])] + [distinct[-1] + 1.0]
    best = None
    for threshold in candidates:
        for direction in ("greater", "less"):
            score = confusion(predict(values, threshold, direction), moving)["balanced_accuracy"]
            if best is None or score > best["train_balanced_accuracy"]:
                best = {"threshold": threshold, "direction": direction, "train_balanced_accuracy": score}
    return best


def score(features_by_file, records):
    """Fit on train intervals, report validation intervals and recordings."""
    rows = {group: [] for group in (FIT_GROUP, SCORE_GROUP)}
    for record, features in zip(records, features_by_file):
        if record["group"] in rows:
            for motion, timing in features:
                rows[record["group"]].append((record, motion, timing))
    labels = {g: [r["activity"] in MOVING for r, _, _ in rows[g]] for g in rows}
    result = {"positive_class": "moving", "moving_codes": "".join(sorted(MOVING))}
    for name, column in (("bfi_motion", 1), ("timing_only", 2)):
        train = [row[column] for row in rows[FIT_GROUP]]
        fitted = fit_threshold(train, labels[FIT_GROUP])
        validation = [row[column] for row in rows[SCORE_GROUP]]
        entry = dict(fitted)
        entry["validation_intervals"] = confusion(
            predict(validation, fitted["threshold"], fitted["direction"]), labels[SCORE_GROUP])
        recordings = collections.defaultdict(list)
        for record, *values in rows[SCORE_GROUP]:
            recordings[record["path"]].append((record["activity"], values[column - 1]))
        names = sorted(recordings)
        recording_values = [statistics.median(v for _, v in recordings[n]) for n in names]
        recording_labels = [recordings[n][0][0] in MOVING for n in names]
        entry["validation_recordings"] = confusion(
            predict(recording_values, fitted["threshold"], fitted["direction"]), recording_labels)
        result[name] = entry
    majority = sum(labels[FIT_GROUP]) * 2 > len(labels[FIT_GROUP])
    result["constant_class"] = {"predicts": "moving" if majority else "still",
                                "validation_intervals": confusion([majority] * len(labels[SCORE_GROUP]),
                                                                  labels[SCORE_GROUP])}
    per_activity = {}
    for group in rows:
        per_activity[group] = {}
        for activity in ACTIVITIES:
            values = [(m, t) for r, m, t in rows[group] if r["activity"] == activity]
            per_activity[group][activity] = {
                "group": "moving" if activity in MOVING else "still",
                "intervals": len(values),
                "median_motion": statistics.median(m for m, _ in values) if values else None,
                "median_reports_per_interval": statistics.median(t for _, t in values) if values else None}
    result["per_activity"] = per_activity
    result["coverage"] = {g: {"eligible_intervals": len(rows[g]),
                              "recordings": len({r["path"] for r, _, _ in rows[g]})} for g in rows}
    return result


# ---------------------------------------------------------------- reporting

def markdown(report):
    lines = [f"# Public BFI motion test ({report['protocol']})", "",
             f"Evidence: **{report['evidence']}** on public data (CSI-BFI-HAR BFI captures only). "
             "Not a presence test, not ESP32 CSI, not a real-home claim.", "",
             f"- Dataset revision: `{report['dataset_revision']}`",
             f"- Manifest SHA-256: `{report['manifest_sha256']}`",
             f"- Repository commit: `{report['repository_commit']}`",
             f"- Workflow run: {report['workflow_run']}",
             f"- Wireshark: {report['tools']['wireshark']}",
             f"- Fragment policy: {report.get('fragment_policy') or 'none (nonzero fragment numbers quarantined)'}",
             "",
             "## Quality census", "",
             "| Group | Files | Admitted files | With eligible interval | Eligible intervals | Eligible seconds |",
             "|---|---|---|---|---|---|"]
    for group, row in report["census"]["groups"].items():
        if not row["downloaded"]:
            lines.append(f"| {group} | not downloaded | | | | |")
            continue
        lines.append(f"| {group} | {row['files']} | {row['admitted_files']} | "
                     f"{row['files_with_eligible_intervals']} | {row['eligible_intervals']} | "
                     f"{row['eligible_seconds']:.0f} |")
    lines += ["", "Recordings with at least one eligible interval, per activity (admission needs 2):", "",
              "| Group | " + " | ".join(ACTIVITIES) + " |", "|---|" + "---|" * len(ACTIVITIES)]
    for group, row in report["census"]["groups"].items():
        coverage = row["recordings_with_eligible_interval_by_activity"]
        lines.append(f"| {group} | " + " | ".join(str(coverage[a]) for a in ACTIVITIES) + " |")
    lines += ["", "Packet outcomes per group:", ""]
    for group, row in report["census"]["groups"].items():
        if row["downloaded"]:
            outcomes = ", ".join(f"{k} {v}" for k, v in row["packet_outcomes"].items()) or "none"
            rejected = ", ".join(f"{k} {v}" for k, v in row["rejected_files"].items()) or "none"
            lines.append(f"- **{group}**: {outcomes}. Rejected files: {rejected}.")
    gate = report["admission"]
    oracle = gate["oracle"]
    lines += ["", "## Decoder oracle", "",
              f"{oracle['reports_compared']} admitted reports from {oracle['files_compared']} files compared "
              f"with {report['tools']['wireshark'].rstrip('.')}"
              + (f", {oracle['policy_reports_compared']} of them admitted under the fragment policy"
                 if "policy_reports_compared" in oracle else "")
              + f". Failures: {oracle['failures'] or 'none'}. "
              "Frequency axis not validated.", "",
              "## Benchmark admission", "",
              f"**{'ADMITTED' if gate['admitted'] else 'NOT ADMITTED'}**"
              + ("" if gate["admitted"] else f": {', '.join(gate['reasons'])}. No detector was scored."), ""]
    if gate["shortfalls"]:
        lines.append(f"{len(gate['shortfalls'])} group/activity cells have fewer than {MIN_RECORDINGS} "
                     "recordings with an eligible interval (see the coverage table).")
        lines.append("")
    scores = report["scores"]
    if scores:
        lines += ["## Scores (validation; threshold fitted on train only)", "",
                  "| Detector | Threshold | Train BA | Validation interval BA | TP/FN/TN/FP | Validation recording BA | TP/FN/TN/FP |",
                  "|---|---|---|---|---|---|---|"]
        for name in ("bfi_motion", "timing_only"):
            s = scores[name]
            vi, vr = s["validation_intervals"], s["validation_recordings"]
            lines.append(f"| {name} | {s['direction']} {s['threshold']:.6g} | {s['train_balanced_accuracy']:.3f} | "
                         f"{vi['balanced_accuracy']:.3f} | {vi['tp']}/{vi['fn']}/{vi['tn']}/{vi['fp']} | "
                         f"{vr['balanced_accuracy']:.3f} | {vr['tp']}/{vr['fn']}/{vr['tn']}/{vr['fp']} |")
        c = scores["constant_class"]["validation_intervals"]
        lines += [f"| constant ({scores['constant_class']['predicts']}) | | | {c['balanced_accuracy']:.3f} | "
                  f"{c['tp']}/{c['fn']}/{c['tn']}/{c['fp']} | | |", "",
                  "Per-activity median motion feature (radians) and eligible intervals:", "",
                  "| Activity | Group | Train median | Train n | Validation median | Validation n |",
                  "|---|---|---|---|---|---|"]
        for activity in ACTIVITIES:
            t, v = scores["per_activity"][FIT_GROUP][activity], scores["per_activity"][SCORE_GROUP][activity]
            fmt = lambda x: "—" if x is None else f"{x:.4f}"  # noqa: E731
            lines.append(f"| {activity} | {t['group']} | {fmt(t['median_motion'])} | {t['intervals']} | "
                         f"{fmt(v['median_motion'])} | {v['intervals']} |")
        lines.append("")
    lines += ["## Limitations", ""] + [f"- {item}" for item in report["limitations"]] + [""]
    return "\n".join(lines)


LIMITATIONS = [
    "Every recording has a person present, so presence detection is not tested.",
    "Moving/still grouping of author activity codes is a frozen judgement call; per-activity numbers allow regrouping.",
    "Train (kitchen, day 1) and validation (classroom, day 3) differ in room and day; they are confounded.",
    "Held-out day-5 groups were inspected for quality only and never scored.",
    "BFI only: CSI is outside the pinned ADR-365 selection, so no BFI-versus-CSI comparison is made.",
    "FRAGMENT_LIMITATION",
    "Direction uses each file's most common transmitter/receiver pair, not a configured AP/client.",
    "Tone positions are ordinals; the frequency axis is not validated.",
]


def limitations(protocol):
    fragment = {
        PROTOCOLS[0]: "Nonzero-fragment frames are quarantined, as ADR-365 requires; this can reduce coverage.",
        PROTOCOLS[1]: f"Nonzero-fragment frames are admitted only under the named policy {FRAGMENT_POLICY} "
                      "(ADR-427): complete, CRC-checked Action No Ack reports without fragment siblings. "
                      "Not comparable to version 1.",
        PROTOCOLS[2]: f"Nonzero-fragment frames are admitted only under the named policy {COUNTER_POLICY} "
                      "(ADR-428): complete, CRC-checked Action No Ack reports, in files with no More "
                      "Fragments frame, without a repeated 16-bit Sequence Control value. Not comparable "
                      "to versions 1 or 2.",
    }[protocol]
    return [fragment if item == "FRAGMENT_LIMITATION" else item for item in LIMITATIONS]


def run(root, tools, commit, run_url, log=print, protocol=PROTOCOL):
    require(protocol in PROTOCOLS, "unknown_protocol")
    policy_name = POLICIES[protocol]
    policy = policy_name is not None
    manifest, _ = download.load_manifest(root / "source-manifest.json")
    receipt = json.loads(download.read_bounded(root / "download-receipt.json", download.MAX_RECEIPT))
    require(receipt.get("status") == "COMPLETED" and receipt.get("manifest_sha256") == download.MANIFEST_SHA256,
            "download_receipt_not_completed")
    verified = {f["path"] for f in receipt.get("files", [])
                if f.get("status") in ("DOWNLOADED", "VERIFIED_EXISTING")}
    entries = [e for e in manifest["files"] if e["path"] in verified]
    downloaded_groups = {e["split"] for e in entries}
    started = time.monotonic()
    records, features = [], []
    with tempfile.TemporaryDirectory(prefix="bfi-motion-") as workdir:
        os.chmod(workdir, 0o700)
        for number, entry in enumerate(entries, 1):
            record, file_features = census_file(entry, download.destination(root, entry["path"]), tools, workdir,
                                                fragment_policy=policy_name == FRAGMENT_POLICY,
                                                counter_policy=policy_name == COUNTER_POLICY)
            records.append(record)
            features.append(file_features)
            detail = record.get("reason") or f"eligible_intervals={record.get('eligible_intervals', 0)}"
            log(f"[{number}/{len(entries)}] {entry['path']}: {record['status']} {detail}")
    groups = summarize(records, downloaded_groups)
    gate = admission(groups, records)
    scores = score(features, records) if gate["admitted"] else None
    if policy:
        policy_compared = sum(r.get("oracle", {}).get("policy_compared", 0) for r in records)
        gate["oracle"]["policy_reports_compared"] = policy_compared
        if policy_compared == 0:
            gate["reasons"].append("fragment_policy_oracle_compared_nothing")
            gate["admitted"] = False
            scores = None
    return {"protocol": protocol, "fragment_policy": policy_name,
            "evidence": "MEASURED",
            "data": "public CSI-BFI-HAR BFI captures (GPL-3.0 dataset; data not redistributed)",
            "dataset_revision": download.REVISION, "manifest_sha256": download.MANIFEST_SHA256,
            "repository_commit": commit, "workflow_run": run_url,
            "tools": {"wireshark": tools["version"], "editcap": tools.get("editcap_version"),
                      "python": platform.python_version(),
                      "numpy": np.__version__},
            "processing_seconds": round(time.monotonic() - started, 1),
            "census": {"groups": groups, "files": records},
            "admission": gate, "scores": scores, "limitations": limitations(protocol)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", required=True, help="Private download directory outside Git")
    parser.add_argument("--report", required=True, type=Path, help="Markdown summary to write")
    parser.add_argument("--json", required=True, type=Path, help="JSON report to write")
    parser.add_argument("--tshark", default="tshark")
    parser.add_argument("--editcap", default="editcap")
    parser.add_argument("--commit", default=os.environ.get("GITHUB_SHA", "unknown"))
    parser.add_argument("--run-url", default="local run")
    parser.add_argument("--protocol", choices=PROTOCOLS, default=PROTOCOL,
                        help="public-bfi-motion-2 adds the ADR-427 fragment policy; "
                             "public-bfi-motion-3 the ADR-428 Sequence Control counter policy")
    args = parser.parse_args(argv)
    try:
        tools = {name: shutil.which(getattr(args, name)) for name in ("tshark", "editcap")}
        require(all(tools.values()), "wireshark_tools_not_found")
        tools["version"] = tool_version(tools["tshark"], "TShark")
        tools["editcap_version"] = tool_version(tools["editcap"], "Editcap")
        report = run(download.private_root(args.data), tools, args.commit, args.run_url, protocol=args.protocol)
    except (DecodeError, download.DownloadError, OSError, ValueError) as error:
        reason = getattr(error, "reason", None) or getattr(error, "code", None) or str(error)
        print(json.dumps({"status": "ERROR", "reason": reason}))
        return 1
    args.json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    args.report.write_text(markdown(report))
    print(json.dumps({"status": "COMPLETED", "admitted": report["admission"]["admitted"],
                      "reasons": report["admission"]["reasons"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

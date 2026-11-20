#!/usr/bin/env python3
"""Score Gate G2 and the Phase 1 checks on real-home recordings (plan item 2.4).

Applies the frozen scoring rules of docs/validation/real-home-dataset-protocol.md
to three inputs:
- the live room verdict logged by log_outputs.py
- the guarded intervals written by `labels.py check --intervals`
- optionally, the tee capture for the mmWave breathing reference

Python standard library only. Every log and capture file is checked against
its manifest's SHA-256 before use.

Results are MEASURED on one home with one configuration. The report holds
aggregate numbers only. Test days are scored once: a second test scoring needs
--rescore-reason, which is recorded.
"""
from __future__ import annotations

import argparse
import bisect
import hashlib
import json
import statistics
import struct
import sys
from datetime import datetime, timezone
from pathlib import Path

NS = 1_000_000_000
NO_OUTPUT_GAP_NS = 5 * NS
EVENT_MERGE_NS = 60 * NS
WALK_IN_BEFORE_NS = 2 * NS
WALK_IN_AFTER_NS = 10 * NS
WALK_IN_TARGET_S = 2.0
MMWAVE_MAX_AGE_NS = 5 * NS
MMWAVE_BR_RANGE = (6.0, 40.0)
FUSED_MAGIC = 0xC5110004
MINIMUM = {"empty_home_hours": 24.0, "still_minutes_in_area": 120.0, "walk_ins": 30}
TARGETS = {"false_events_per_empty_day": 1.0, "still_held_share": 0.90, "walk_in_latency_s": WALK_IN_TARGET_S}
MAX_FILE = 512 * 1024 * 1024


class ScoreError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise ScoreError(reason)


def to_ns(value):
    require(isinstance(value, str) and value.endswith("Z"), "time_must_be_utc_iso_with_z")
    stamp = datetime.fromisoformat(value[:-1] + "+00:00")
    return int(stamp.timestamp()) * NS + stamp.microsecond * 1000


def verified_files(root, manifest_name, allow_unclosed=False):
    """Files listed in the manifest, hash-checked; plus unlisted ones only if allowed."""
    manifest_path = root / manifest_name
    listed = {}
    if manifest_path.exists():
        for line in manifest_path.read_text().splitlines():
            entry = json.loads(line)
            listed[entry["file"]] = entry["sha256"]
    files, unclosed = [], 0
    pattern = "outputs/**/*.jsonl" if manifest_name == "outputs-manifest.jsonl" else "*/*.pcap"
    for path in sorted(root.glob(pattern)):
        relative = path.relative_to(root).as_posix()
        if relative not in listed:
            unclosed += 1
            require(allow_unclosed, f"unclosed_or_unlisted_file:{relative}")
            files.append(path)
            continue
        require(path.stat().st_size <= MAX_FILE, "file_too_large")
        require(hashlib.sha256(path.read_bytes()).hexdigest() == listed[relative], f"hash_mismatch:{relative}")
        files.append(path)
    return files, {"files": len(files), "unclosed": unclosed}


class Timeline:
    """Room verdict over time: True, False, or None (no output)."""

    def __init__(self, records):
        points = []
        for record in records:
            if "polled_ns" not in record or "links" in record:
                continue
            value = record["presence"] if "tick" in record else None
            points.append((int(record["polled_ns"]), value))
        points.sort(key=lambda p: p[0])
        self.times, self.values = [p[0] for p in points], [p[1] for p in points]

    def pieces(self, start, end):
        """(s, e, value) covering [start, end). Record i holds until the next record,
        but for at most 5 s; anything not covered is None (no output)."""
        out, cursor = [], start
        index = bisect.bisect_right(self.times, start) - 1
        while cursor < end:
            if index < 0:
                stop = min(end, self.times[0]) if self.times else end
                out.append((cursor, stop, None))
                cursor, index = stop, 0
                continue
            time, value = self.times[index], self.values[index]
            nxt = self.times[index + 1] if index + 1 < len(self.times) else None
            covered = time + NO_OUTPUT_GAP_NS if nxt is None else min(nxt, time + NO_OUTPUT_GAP_NS)
            if cursor < covered:
                stop = min(end, covered)
                out.append((cursor, stop, value))
                cursor = stop
            gap_end = end if nxt is None else min(end, nxt)
            if cursor < gap_end:
                out.append((cursor, gap_end, None))
                cursor = gap_end
            if nxt is None:
                break
            index += 1
        return [(s, e, v) for s, e, v in out if e > s]

    def value_at(self, time):
        index = bisect.bisect_right(self.times, time) - 1
        if index < 0 or time - self.times[index] > NO_OUTPUT_GAP_NS:
            return None
        return self.values[index]


def durations(pieces):
    present = sum(e - s for s, e, v in pieces if v is True)
    absent = sum(e - s for s, e, v in pieces if v is False)
    missing = sum(e - s for s, e, v in pieces if v is None)
    return present, absent, missing


def false_events(pieces):
    """Events in one guarded span: present at its start or absent-to-present, merged within 60 s."""
    events, last_end, previous = 0, None, None
    for start, end, value in pieces:
        if value is True and previous is not True:
            if last_end is None or start - last_end >= EVENT_MERGE_NS:
                events += 1
        if value is True:
            last_end = end
        if value is not None:
            previous = value
    return events


def walk_in_result(timeline, reference):
    before = timeline.value_at(reference - WALK_IN_BEFORE_NS)
    if before is None:
        return "no_output", None
    if before is True:
        return "already_present", None
    index = bisect.bisect_right(timeline.times, reference - WALK_IN_BEFORE_NS)
    while index < len(timeline.times) and timeline.times[index] <= reference + WALK_IN_AFTER_NS:
        if timeline.values[index] is True:
            return "detected", (timeline.times[index] - reference) / NS
        index += 1
    return "missed", None


def mmwave_breathing(capture_files):
    """(time_ns, node, bpm) from ADR-063 fused vitals packets with mmWave presence."""
    out = []
    for path in capture_files:
        raw = path.read_bytes()
        require(raw[:4] == struct.pack("<I", 0xA1B23C4D) and struct.unpack_from("<I", raw, 20)[0] == 147,
                f"not_a_tee_capture:{path.name}")
        offset = 24
        while offset + 16 <= len(raw):
            seconds, fraction, captured, _ = struct.unpack_from("<IIII", raw, offset)
            data = raw[offset + 16:offset + 16 + captured]
            offset += 16 + captured
            if len(data) == 48 and struct.unpack_from("<I", data)[0] == FUSED_MAGIC and data[5] & 0x08:
                bpm = struct.unpack_from("<f", data, 32)[0]
                if MMWAVE_BR_RANGE[0] <= bpm <= MMWAVE_BR_RANGE[1]:
                    out.append((seconds * NS + fraction, data[4], bpm))
    out.sort()
    return out


def share(numerator, denominator):
    return None if denominator <= 0 else numerator / denominator


def score(records, scoring, split, mmwave=None):
    timeline = Timeline(records)
    intervals = [i for i in scoring["intervals"] if i["split"] == split]
    walk_ins = [w for w in scoring["walk_ins"] if w["split"] == split]
    by_state = {}
    for item in intervals:
        key = item["state"] if item["state"] != "still" else ("still_in_area" if item.get("in_area", True)
                                                               else "still_outside")
        by_state.setdefault(key, []).append((to_ns(item["start"]), to_ns(item["end"])))

    def held(key):
        present = scored = missing = 0
        for start, end in by_state.get(key, []):
            p, a, m = durations(timeline.pieces(start, end))
            present, scored, missing = present + p, scored + p + a, missing + m
        return {"held_share": share(present, scored), "scored_minutes": scored / NS / 60,
                "no_output_minutes": missing / NS / 60}

    def events(key):
        count = scored = missing = 0
        for start, end in by_state.get(key, []):
            pieces = timeline.pieces(start, end)
            p, a, m = durations(pieces)
            count, scored, missing = count + false_events(pieces), scored + p + a, missing + m
        hours = scored / NS / 3600
        return {"events": count, "scored_hours": hours, "no_output_hours": missing / NS / 3600,
                "events_per_day": None if hours <= 0 else count / hours * 24,
                "events_per_hour": None if hours <= 0 else count / hours}

    empty = events("home_empty")
    room_empty = events("room_empty")
    still = held("still_in_area")
    walks = {"detected": [], "already_present": 0, "missed": 0, "no_output": 0}
    for walk in walk_ins:
        outcome, latency = walk_in_result(timeline, to_ns(walk["time"]))
        if outcome == "detected":
            walks["detected"].append(latency)
        else:
            walks[outcome] += 1
    latencies = sorted(walks["detected"])
    counted = len(walk_ins) - walks["no_output"]
    within = sum(1 for x in latencies if x <= WALK_IN_TARGET_S)
    walk_summary = {"walk_ins": len(walk_ins), "scored": counted, "detected": len(latencies),
                    "within_2s": within, "already_present": walks["already_present"],
                    "missed": walks["missed"], "no_output": walks["no_output"],
                    "median_s": statistics.median(latencies) if latencies else None,
                    "p90_s": latencies[min(len(latencies) - 1, int(0.9 * len(latencies)))] if latencies else None,
                    "max_s": latencies[-1] if latencies else None}
    pet = by_state.get("pet_only", [])
    pet_raised = sum(1 for s, e in pet if any(v is True for _, _, v in timeline.pieces(s, e)))
    breathing = None
    if mmwave is not None:
        times = [m[0] for m in mmwave]
        errors, logged = [], 0
        spans = by_state.get("still_in_area", []) + by_state.get("still_outside", []) + by_state.get("sleep", [])
        for record in records:
            if "tick" not in record or record.get("breathing_bpm") is None:
                continue
            t = int(record["polled_ns"])
            if not any(s <= t < e for s, e in spans):
                continue
            logged += 1
            index = bisect.bisect_right(times, t) - 1
            if index >= 0 and t - times[index] <= MMWAVE_MAX_AGE_NS:
                errors.append(abs(record["breathing_bpm"] - mmwave[index][2]))
        breathing = {"pairs": len(errors), "estimates_in_still_or_sleep": logged,
                     "median_abs_error_bpm": statistics.median(errors) if errors else None,
                     "share_within_0_5_bpm": share(sum(e <= 0.5 for e in errors), len(errors))}
    minimum = {"empty_home_hours": empty["scored_hours"] + empty["no_output_hours"],
               "still_minutes_in_area": still["scored_minutes"] + still["no_output_minutes"],
               "walk_ins": len(walk_ins)}
    missing = {k: {"have": round(minimum[k], 2), "need": v} for k, v in MINIMUM.items() if minimum[k] < v}
    gate = None
    if split == "test" and not missing:
        gate = {"false_events_per_empty_day": empty["events_per_day"] <= TARGETS["false_events_per_empty_day"],
                "still_held_share": (still["held_share"] or 0.0) >= TARGETS["still_held_share"],
                "walk_in_latency": counted > 0 and within == counted}
        gate["passed"] = all(gate.values())
    return {"split": split, "empty_home": empty, "room_empty_home_occupied": room_empty,
            "still_in_area": still, "still_outside_area": held("still_outside"), "sleep": held("sleep"),
            "walk_ins": walk_summary, "pet_only": {"episodes": len(pet), "raised_occupied": pet_raised},
            "breathing_vs_mmwave": breathing, "minimum_test_data": {"met": not missing, "missing": missing},
            "g2": gate if gate is not None else ("insufficient data" if split == "test" else "train split: not a G2 result")}


def fmt(value, digits=2, pct=False):
    if value is None:
        return "—"
    return f"{value * 100:.1f}%" if pct else f"{value:.{digits}f}"


def markdown(report):
    r = report["results"]
    w = r["walk_ins"]
    lines = [f"# Real-home scoring ({r['split']} days)", "",
             f"Evidence: **MEASURED** on one home, one configuration. Freeze commit `{report['freeze_commit']}`. "
             f"Scored {report['scored_utc']}.", "",
             "| Metric | Result | G2 target |", "|---|---|---|",
             f"| False occupied events per empty-home day | {fmt(r['empty_home']['events_per_day'])} "
             f"({r['empty_home']['events']} in {fmt(r['empty_home']['scored_hours'], 1)} h) | ≤ 1 |",
             f"| Still person held (inside sensing area) | {fmt(r['still_in_area']['held_share'], pct=True)} "
             f"of {fmt(r['still_in_area']['scored_minutes'], 0)} min | ≥ 90% |",
             f"| Walk-ins within 2 s | {w['within_2s']} of {w['scored']} (median {fmt(w['median_s'])} s, "
             f"p90 {fmt(w['p90_s'])} s; already present {w['already_present']}, missed {w['missed']}) | all |",
             "", "Also reported:", "",
             f"- Room empty, people elsewhere in the home: {fmt(r['room_empty_home_occupied']['events_per_hour'])} "
             "false events per hour",
             f"- Still outside the sensing area: held {fmt(r['still_outside_area']['held_share'], pct=True)}",
             f"- Asleep: held {fmt(r['sleep']['held_share'], pct=True)}",
             f"- Pet-only episodes that raised occupied: {r['pet_only']['raised_occupied']} of "
             f"{r['pet_only']['episodes']}"]
    if r["breathing_vs_mmwave"]:
        b = r["breathing_vs_mmwave"]
        lines.append(f"- Breathing vs mmWave reference: median error {fmt(b['median_abs_error_bpm'])} BPM over "
                     f"{b['pairs']} pairs ({fmt(b['share_within_0_5_bpm'], pct=True)} within 0.5 BPM)")
    gate = r["g2"]
    verdict = gate if isinstance(gate, str) else ("PASSED" if gate["passed"] else "NOT PASSED")
    lines += ["", f"**G2: {verdict}**", ""]
    if r["minimum_test_data"]["missing"]:
        lines.append("Missing test data: " + ", ".join(f"{k} {v['have']} of {v['need']}"
                                                       for k, v in r["minimum_test_data"]["missing"].items()))
    lines += ["", "Per-frame processing time is not measured by this tool.", ""]
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data", required=True, type=Path, help="Private study directory (recorder + logger output)")
    parser.add_argument("--intervals", required=True, type=Path, help="labels.py check --intervals output")
    parser.add_argument("--split", choices=("train", "test"), default="test")
    parser.add_argument("--freeze-commit", help="Commit frozen before test scoring (required for --split test)")
    parser.add_argument("--rescore-reason", help="Required to score the test days a second time; recorded")
    parser.add_argument("--allow-unclosed", action="store_true", help="Include files without a manifest entry")
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--json", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        root = args.data.resolve()
        marker = root / "scoring" / "test-scored.json"
        if args.split == "test":
            require(bool(args.freeze_commit), "freeze_commit_required_for_test_split")
            require(not marker.exists() or bool(args.rescore_reason), "test_days_already_scored")
        outputs, output_check = verified_files(root, "outputs-manifest.jsonl", args.allow_unclosed)
        records = [json.loads(line) for path in outputs for line in path.read_text().splitlines() if line]
        require(records, "no_output_records")
        captures, capture_check = verified_files(root, "manifest.jsonl", args.allow_unclosed)
        scoring = json.loads(args.intervals.read_text())
        results = score(records, scoring, args.split, mmwave_breathing(captures) if captures else None)
    except (ScoreError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1
    report = {"evidence": "MEASURED", "protocol": "docs/validation/real-home-dataset-protocol.md",
              "freeze_commit": args.freeze_commit or "not frozen (train split)",
              "scored_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
              "inputs": {"outputs": output_check, "captures": capture_check,
                         "intervals_sha256": hashlib.sha256(args.intervals.read_bytes()).hexdigest()},
              "rescore_reason": args.rescore_reason, "results": results}
    args.json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    args.report.write_text(markdown(report))
    if args.split == "test":
        marker.parent.mkdir(mode=0o700, exist_ok=True)
        history = json.loads(marker.read_text()) if marker.exists() else []
        history.append({"scored_utc": report["scored_utc"], "freeze_commit": args.freeze_commit,
                        "rescore_reason": args.rescore_reason})
        marker.write_text(json.dumps(history, indent=1) + "\n")
    print(json.dumps({"status": "SCORED", "split": args.split, "g2": results["g2"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

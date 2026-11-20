#!/usr/bin/env python3
"""Score the firmware fall detector on staged sessions (docs/validation/fall-study-protocol.md).

Python standard library only. Reads the fall bit (flags bit 1) from vitals
(0xC5110002) and fused vitals (0xC5110004) packets in the hash-checked tee
capture. Applies the frozen windows to the session CSVs:
- detected falls: an edge from 1 s before to 5 s after the impact press
- fall-like trials that raised an alarm, from the cue to 10 s after the press
- false alarms per hour in normal activity

Optionally also scores node log detections from a CSV of "Fall detected!"
times. Reports Wilson 95% intervals and aggregate numbers only, tagged
MEASURED.
"""
from __future__ import annotations

import argparse
import bisect
import csv
import json
import math
import struct
import sys
from pathlib import Path

import score_g2

NS = score_g2.NS
VITALS_MAGIC, FUSED_MAGIC = 0xC5110002, 0xC5110004
BEFORE_NS, AFTER_NS, FALL_LIKE_AFTER_NS = 1 * NS, 5 * NS, 10 * NS
MINIMUM = {"completed_falls": 30, "fall_like_trials": 30, "activity_minutes": 60.0}


def fall_edges(capture_files, spans=None):
    """Rising edges of the fall bit, per node, as sorted nanosecond times.

    If `spans` is a list, each capture file's first-to-last vitals packet span is appended to it.
    """
    previous, edges = {}, []
    packets = []
    for path in capture_files:
        raw = path.read_bytes()
        score_g2.require(len(raw) >= 24 and raw[:4] == struct.pack("<I", 0xA1B23C4D)
                         and struct.unpack_from("<I", raw, 20)[0] == 147, f"not_a_tee_capture:{path.name}")
        offset, first = 24, len(packets)
        while offset < len(raw):
            score_g2.require(offset + 16 <= len(raw), f"truncated_capture_record:{path.name}")
            seconds, fraction, captured, _ = struct.unpack_from("<IIII", raw, offset)
            score_g2.require(offset + 16 + captured <= len(raw), f"truncated_capture_record:{path.name}")
            data = raw[offset + 16:offset + 16 + captured]
            offset += 16 + captured
            if len(data) >= 32 and struct.unpack_from("<I", data)[0] in (VITALS_MAGIC, FUSED_MAGIC):
                packets.append((seconds * NS + fraction, data[4], bool(data[5] & 0x02)))
        if spans is not None and len(packets) > first:
            spans.append((packets[first][0], packets[-1][0]))
    for time, node, fall in sorted(packets):
        if fall and not previous.get(node, False):
            edges.append(time)
        previous[node] = fall
    return edges


def wilson(successes, trials, z=1.96):
    if trials == 0:
        return None
    p = successes / trials
    centre = (p + z * z / (2 * trials)) / (1 + z * z / trials)
    half = z * math.sqrt(p * (1 - p) / trials + z * z / (4 * trials * trials)) / (1 + z * z / trials)
    return [max(0.0, centre - half), min(1.0, centre + half)]


def any_between(edges, start, end):
    index = bisect.bisect_left(edges, start)
    return index < len(edges) and edges[index] <= end


def daily_life(rows, edges, spans, margin_ns=60 * NS):
    """False alarms per day over the capture, outside every session (with a 60 s margin)."""
    sessions = {}
    for row in rows:
        times = [score_g2.to_ns(row[k]) for k in ("cue_utc", "mark_utc", "end_utc") if row.get(k)]
        if times:
            low, high = sessions.get(row.get("session"), (min(times), max(times)))
            sessions[row.get("session")] = (min(low, *times), max(high, *times))
    cuts = [(low - margin_ns, high + margin_ns) for low, high in sessions.values()]
    covered = sum(sum(e - s for s, e in score_g2.subtract_spans((start, end), cuts)) for start, end in spans)
    alarms = sum(1 for t in edges if not any(s <= t <= e for s, e in cuts))
    days = covered / NS / 86400
    return {"days": days, "false_alarms": alarms, "per_day": None if days <= 0 else alarms / days}


def score(rows, edges):
    falls = [r for r in rows if r["group"] == "fall" and r["status"] == "completed"]
    fall_like = [r for r in rows if r["group"] == "fall_like" and r["status"] == "completed"]
    activity = [r for r in rows if r["group"] == "activity" and r["status"] == "completed"]
    aborted = sum(1 for r in rows if r["status"] == "aborted")
    detected, per_kind = 0, {}
    for row in falls:
        mark = score_g2.to_ns(row["mark_utc"])
        hit = any_between(edges, mark - BEFORE_NS, mark + AFTER_NS)
        detected += hit
        kind = per_kind.setdefault(row["kind"], {"trials": 0, "detected": 0})
        kind["trials"] += 1
        kind["detected"] += hit
    raised, like_kind = 0, {}
    for row in fall_like:
        hit = any_between(edges, score_g2.to_ns(row["cue_utc"]), score_g2.to_ns(row["mark_utc"]) + FALL_LIKE_AFTER_NS)
        raised += hit
        kind = like_kind.setdefault(row["kind"], {"trials": 0, "raised": 0})
        kind["trials"] += 1
        kind["raised"] += hit
    alarms, seconds = 0, 0.0
    for row in activity:
        start, end = score_g2.to_ns(row["cue_utc"]), score_g2.to_ns(row["end_utc"])
        seconds += (end - start) / NS
        alarms += bisect.bisect_right(edges, end) - bisect.bisect_left(edges, start)
    totals = {"completed_falls": len(falls), "fall_like_trials": len(fall_like), "activity_minutes": seconds / 60}
    missing = {k: {"have": round(totals[k], 1), "need": v} for k, v in MINIMUM.items() if totals[k] < v}
    return {"falls": {"completed": len(falls), "detected": detected,
                      "detection_rate": None if not falls else detected / len(falls),
                      "wilson_95": wilson(detected, len(falls)), "per_kind": per_kind},
            "fall_like": {"trials": len(fall_like), "raised_alarm": raised,
                          "share": None if not fall_like else raised / len(fall_like),
                          "wilson_95": wilson(raised, len(fall_like)), "per_kind": like_kind},
            "normal_activity": {"minutes": seconds / 60, "false_alarms": alarms,
                                "per_hour": None if seconds <= 0 else alarms / seconds * 3600},
            "aborted_trials": aborted, "minimum": {"met": not missing, "missing": missing}}


def read_sessions(paths):
    rows = []
    for path in paths:
        with open(path, newline="", encoding="utf-8") as stream:
            reader = csv.DictReader(stream)
            score_g2.require(set(reader.fieldnames or ()) >= {"group", "kind", "cue_utc", "mark_utc", "end_utc",
                                                              "status"}, f"not_a_session_file:{path}")
            rows += list(reader)
    return rows


def log_edges(path):
    """Times of node log detections: a CSV with a time_utc column, one row per 'Fall detected!' line."""
    with open(path, newline="", encoding="utf-8") as stream:
        return sorted(score_g2.to_ns(row["time_utc"]) for row in csv.DictReader(stream))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data", required=True, type=Path, help="Private directory with the tee capture")
    parser.add_argument("--sessions", required=True, nargs="+", type=Path, help="fall_session.py CSV files")
    parser.add_argument("--log-detections", type=Path, help="CSV of node log 'Fall detected!' times (time_utc)")
    parser.add_argument("--allow-unclosed", action="store_true")
    parser.add_argument("--json", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        captures, check = score_g2.verified_files(args.data.resolve(), "manifest.jsonl", args.allow_unclosed)
        rows = read_sessions(args.sessions)
        spans = []
        edges = fall_edges(captures, spans)
        report = {"evidence": "MEASURED", "protocol": "docs/validation/fall-study-protocol.md",
                  "inputs": {"captures": check, "sessions": len(args.sessions)},
                  "packet_detections": score(rows, edges),
                  "daily_life_packet_false_alarms": daily_life(rows, edges, spans)}
        if args.log_detections:
            report["log_detections"] = score(rows, log_edges(args.log_detections))
    except (score_g2.ScoreError, OSError, ValueError, KeyError, struct.error) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1
    args.json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    falls = report["packet_detections"]["falls"]
    print(json.dumps({"status": "SCORED", "falls_detected": f"{falls['detected']} of {falls['completed']}",
                      "minimum_met": report["packet_detections"]["minimum"]["met"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

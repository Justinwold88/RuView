"""SYNTHETIC tests for the fall-study session runner and scorer. No hardware, no captured data."""
import csv
import hashlib
import json
import struct
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

import fall_session
import score_falls

NS = score_falls.NS
BASE = 1_796_000_400  # seconds, 2026-11-30T01:00:00Z


def iso(seconds):
    return datetime.fromtimestamp(BASE + seconds, tz=timezone.utc).isoformat(timespec="milliseconds").replace(
        "+00:00", "Z")


def vitals(node, fall, magic=score_falls.VITALS_MAGIC, size=32):
    data = bytearray(size)
    struct.pack_into("<IBB", data, 0, magic, node, 0x02 if fall else 0x01)
    return bytes(data)


def capture(packets):
    raw = struct.pack("<IHHiIII", 0xA1B23C4D, 2, 4, 0, 0, 65535, 147)
    for seconds, datagram in packets:
        whole = int(seconds)
        raw += struct.pack("<IIII", BASE + whole, int((seconds - whole) * 1e9), len(datagram),
                           len(datagram)) + datagram
    return raw


class SessionTests(unittest.TestCase):
    def test_plan_is_frozen_and_seeded(self):
        trials = fall_session.plan(7)
        self.assertEqual(len(trials), 12 + 15 + 2)
        self.assertEqual(trials, fall_session.plan(7))
        self.assertNotEqual(trials[:27], fall_session.plan(8)[:27])
        self.assertEqual(trials[-2:], [("walk_around", "activity"), ("chores", "activity")])
        self.assertFalse(any(group == "fall" for _, group in fall_session.plan(7, falls=False)))

    def test_run_writes_one_row_per_trial_and_aborts(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.csv"
            answers = iter(["", "a"] + [""] * 200)
            ticks = iter(range(1000))
            count = fall_session.run(path, "F1", "P1", 3, falls=False, ask=lambda _: next(answers),
                                     clock=lambda: iso(next(ticks)), wait=lambda _: None, say=lambda *_: None)
            rows = list(csv.DictReader(path.open()))
            self.assertEqual(count, len(rows))
            self.assertEqual(rows[0]["status"], "aborted")
            self.assertEqual({r["status"] for r in rows[1:]}, {"completed"})
            self.assertTrue(all(r["end_utc"] for r in rows if r["group"] == "activity"))
        with self.assertRaises(fall_session.SessionError):
            fall_session.run(Path(__file__).resolve().parent / "s.csv", "F1", "P1", 1)


def row(kind, group, cue, mark=None, end=None, status="completed", session="F1"):
    return {"session": session, "kind": kind, "group": group, "cue_utc": iso(cue),
            "mark_utc": iso(mark) if mark is not None else "", "end_utc": iso(end) if end is not None else "",
            "status": status}


class ScoreTests(unittest.TestCase):
    def test_windows_rates_and_intervals(self):
        rows = [row("fall_forward", "fall", 0, 3), row("fall_backward", "fall", 30, 33),
                row("fall_sideways", "fall", 60, 63, status="aborted"),
                row("sit_fast", "fall_like", 90, 93), row("drop_object", "fall_like", 120, 123),
                row("walk_around", "activity", 200, end=800)]
        edges = [BASE * NS + int(t * NS) for t in (2.5, 38.5, 100, 300, 700)]
        result = score_falls.score(rows, edges)
        self.assertEqual((result["falls"]["completed"], result["falls"]["detected"]), (2, 1))
        self.assertEqual(result["falls"]["per_kind"]["fall_forward"], {"trials": 1, "detected": 1})
        self.assertEqual(result["fall_like"]["raised_alarm"], 1)
        self.assertEqual(result["normal_activity"]["false_alarms"], 2)
        self.assertAlmostEqual(result["normal_activity"]["per_hour"], 12.0)
        self.assertEqual(result["aborted_trials"], 1)
        self.assertFalse(result["minimum"]["met"])
        low, high = score_falls.wilson(1, 2)
        self.assertTrue(0.0 < low < 0.5 < high < 1.0)
        self.assertIsNone(score_falls.wilson(0, 0))

    def test_edges_come_from_rising_fall_bits_per_node(self):
        packets = [(0, vitals(1, False)), (1, vitals(1, True)), (1.1, vitals(2, True)), (2, vitals(1, True)),
                   (3, vitals(1, False)), (4, vitals(1, True, score_falls.FUSED_MAGIC, 48)),
                   (5, b"\x01\x00\x11\xc5" + bytes(40))]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "x.pcap"
            path.write_bytes(capture(packets))
            spans = []
            edges = score_falls.fall_edges([path], spans)
        self.assertEqual([round(e / NS - BASE, 1) for e in edges], [1.0, 1.1, 4.0])
        self.assertEqual(spans, [(BASE * NS, (BASE + 4) * NS)])

    def test_daily_life_excludes_sessions(self):
        rows = [row("fall_forward", "fall", 1000, 1003, end=None)]
        spans = [(BASE * NS, (BASE + 86400) * NS)]
        edges = [(BASE + t) * NS for t in (500, 1002, 50_000)]
        result = score_falls.daily_life(rows, edges, spans)
        self.assertEqual(result["false_alarms"], 2)
        self.assertAlmostEqual(result["days"], (86400 - 123) / 86400)

    def test_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "home"
            (root / "2026-11-30").mkdir(parents=True)
            raw = capture([(0, vitals(1, False)), (3.5, vitals(1, True))])
            (root / "2026-11-30/2026-11-30T01Z.pcap").write_bytes(raw)
            (root / "manifest.jsonl").write_text(json.dumps(
                {"file": "2026-11-30/2026-11-30T01Z.pcap", "sha256": hashlib.sha256(raw).hexdigest()}) + "\n")
            session = Path(directory) / "s.csv"
            with session.open("w", newline="") as stream:
                writer = csv.DictWriter(stream, fieldnames=fall_session.FIELDS)
                writer.writeheader()
                writer.writerow(dict(dict.fromkeys(fall_session.FIELDS, ""), **row("fall_forward", "fall", 0, 3)))
            out = Path(directory) / "falls.json"
            self.assertEqual(score_falls.main(["--data", str(root), "--sessions", str(session),
                                               "--json", str(out)]), 0)
            report = json.loads(out.read_text())
            self.assertEqual(report["packet_detections"]["falls"]["detected"], 1)
            self.assertEqual(report["evidence"], "MEASURED")


if __name__ == "__main__":
    unittest.main()

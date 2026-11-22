"""SYNTHETIC tests for G2 scoring. No hardware, no captured data."""
import hashlib
import json
import struct
import tempfile
import unittest
from pathlib import Path

import score_g2 as g2

NS = g2.NS
BASE = 1_796_000_400 * NS  # 2026-11-30T01:00:00Z


def iso(ns):
    from datetime import datetime, timezone
    return datetime.fromtimestamp(ns / NS, tz=timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def verdicts(spans, step=0.25):
    """[(start_s, end_s, presence)] -> logged records every `step` seconds."""
    records, tick = [], 0
    for start, end, presence in spans:
        t = start
        while t < end:
            tick += 1
            records.append({"polled_ns": BASE + int(t * NS), "tick": tick, "presence": presence,
                            "breathing_bpm": 14.0 if presence else None})
            t += step
    return records


def interval(state, start, end, split="test", **extra):
    return dict({"room": "study", "state": state, "spot": None, "day": 4, "split": split,
                 "start": iso(BASE + int(start * NS)), "end": iso(BASE + int(end * NS))}, **extra)


class TimelineTests(unittest.TestCase):
    def test_false_events_merge_within_60s_and_count_present_at_start(self):
        records = verdicts([(0, 100, False), (100, 110, True), (110, 140, False), (140, 145, True),
                            (145, 445, False), (445, 450, True), (450, 600, False)])
        timeline = g2.Timeline(records)
        self.assertEqual(g2.false_events(timeline.pieces(BASE, BASE + 600 * NS)), 2)
        self.assertEqual(g2.false_events(timeline.pieces(BASE + 105 * NS, BASE + 130 * NS)), 1)
        self.assertEqual(g2.false_events(timeline.pieces(BASE, BASE + 99 * NS)), 0)

    def test_output_gaps_are_no_output(self):
        records = verdicts([(0, 10, True)]) + verdicts([(30, 40, False)])
        records[-1]["tick"] = 10**6
        timeline = g2.Timeline(records + [{"polled_ns": BASE + 40 * NS, "status": "unreachable"}])
        present, absent, missing = g2.durations(timeline.pieces(BASE, BASE + 50 * NS))
        # The last present record (9.75 s) holds for 5 s; 14.75-30 s and 40-50 s have no output.
        self.assertEqual((present, absent, missing), (14_750_000_000, 10 * NS, 25_250_000_000))
        self.assertIsNone(timeline.value_at(BASE + 20 * NS))

    def test_walk_in_outcomes(self):
        timeline = g2.Timeline(verdicts([(0, 101.25, False), (101.25, 200, True)]))
        self.assertEqual(g2.walk_in_result(timeline, BASE + 100 * NS), ("detected", 1.25))
        self.assertEqual(g2.walk_in_result(timeline, BASE + 102 * NS), ("detected", -0.75))
        self.assertEqual(g2.walk_in_result(timeline, BASE + 150 * NS), ("already_present", None))
        timeline = g2.Timeline(verdicts([(0, 200, False)]))
        self.assertEqual(g2.walk_in_result(timeline, BASE + 100 * NS), ("missed", None))
        self.assertEqual(g2.walk_in_result(timeline, BASE + 300 * NS), ("no_output", None))


class ScoreTests(unittest.TestCase):
    def scoring(self, intervals, walk_ins=()):
        return {"intervals": intervals, "walk_ins": [{"room": "study", "time": iso(BASE + int(t * NS)),
                                                     "source": "door", "day": 4, "split": "test"}
                                                    for t in walk_ins]}

    def test_g2_passes_and_fails_on_the_frozen_targets(self):
        hours = 25
        empty = hours * 3600
        spans = [(0, empty, False), (empty, empty + 7300, True)]
        records = verdicts(spans, step=1.0)
        still = [interval("still", empty + 100, empty + 7300, in_area=True)]
        walks = []
        for n in range(30):  # 30 walk-ins after empty time, detected 1 s later
            start = empty + 7300 + 200 * n
            records += verdicts([(start, start + 101, False), (start + 101, start + 200, True)], step=0.5)
            walks.append(start + 100)
        scoring = self.scoring([interval("home_empty", 0, empty)] + still, walks)
        result = g2.score(records, scoring, "test")
        self.assertEqual(result["empty_home"]["events"], 0)
        self.assertEqual(result["still_in_area"]["held_share"], 1.0)
        self.assertEqual(result["walk_ins"]["within_2s"], 30)
        self.assertTrue(result["g2"]["passed"])
        # One walk-in already present fails the walk-in target.
        records = [r for r in records if not (BASE + (empty + 7300) * NS <= r["polled_ns"] < BASE + (empty + 7500) * NS)]
        records += verdicts([(empty + 7300, empty + 7500, True)], step=0.5)
        result = g2.score(records, scoring, "test")
        self.assertEqual(result["walk_ins"]["already_present"], 1)
        self.assertFalse(result["g2"]["walk_in_latency"])
        self.assertFalse(result["g2"]["passed"])

    def test_insufficient_data_and_train_split(self):
        records = verdicts([(0, 3600, False)], step=1.0)
        result = g2.score(records, self.scoring([interval("home_empty", 0, 3600)]), "test")
        self.assertEqual(result["g2"], "insufficient data")
        self.assertIn("walk_ins", result["minimum_test_data"]["missing"])
        train = self.scoring([interval("home_empty", 0, 3600, split="train")])
        self.assertEqual(g2.score(records, train, "train")["empty_home"]["scored_hours"], 1.0)
        self.assertEqual(g2.score(records, train, "test")["empty_home"]["scored_hours"], 0.0)

    def test_pets_rooms_and_breathing(self):
        records = verdicts([(0, 600, False), (600, 900, True)], step=1.0)
        scoring = self.scoring([interval("pet_only", 0, 300), interval("pet_only", 550, 700),
                                interval("room_empty", 0, 500), interval("sleep", 600, 900)])
        mmwave = [(BASE + int(t * NS), 1, 14.5) for t in range(600, 900, 2)]
        result = g2.score(records, scoring, "test", mmwave)
        self.assertEqual(result["pet_only"], {"episodes": 2, "raised_occupied": 1})
        self.assertEqual(result["sleep"]["held_share"], 1.0)
        self.assertEqual(result["breathing_vs_mmwave"]["median_abs_error_bpm"], 0.5)
        self.assertEqual(result["room_empty_home_occupied"]["events"], 0)

    def test_pets_alone_is_reported_apart_from_g2(self):
        # 2 h with nobody home and pets loose; the room reads present twice.
        spans = [(0, 1000, False), (1000, 1100, True), (1100, 4000, False), (4000, 4050, True),
                 (4050, 7200, False)]
        result = g2.score(verdicts(spans, step=1.0), self.scoring([interval("pets_alone", 0, 7200)]), "test")
        self.assertEqual(result["pets_alone"]["events"], 2)
        self.assertAlmostEqual(result["pets_alone"]["events_per_hour"], 1.0)
        self.assertEqual(result["empty_home"]["events"], 0)
        self.assertEqual(result["empty_home"]["scored_hours"], 0.0)
        self.assertIn("empty_home_hours", result["minimum_test_data"]["missing"])
        text = g2.markdown({"results": result, "freeze_commit": "c", "scored_utc": "t"})
        self.assertIn("Nobody home, pets loose: 1.00 false occupied events per hour (2 in 2.0 h)", text)


def fused(node, bpm, present=True):
    data = bytearray(48)
    struct.pack_into("<IBB", data, 0, g2.FUSED_MAGIC, node, 0x08 if present else 0)
    struct.pack_into("<f", data, 32, bpm)
    return bytes(data)


class FileTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name) / "home"
        (self.root / "outputs" / "2026-11-30").mkdir(parents=True)
        (self.root / "2026-11-30").mkdir()

    def tearDown(self):
        self.directory.cleanup()

    def write_inputs(self):
        lines = "".join(json.dumps(r) + "\n" for r in verdicts([(0, 3600, False)], step=1.0))
        output = self.root / "outputs/2026-11-30/2026-11-30T01Z.jsonl"
        output.write_text(lines)
        (self.root / "outputs-manifest.jsonl").write_text(json.dumps(
            {"file": "outputs/2026-11-30/2026-11-30T01Z.jsonl", "sha256": hashlib.sha256(lines.encode()).hexdigest()})
            + "\n")
        capture = struct.pack("<IHHiIII", 0xA1B23C4D, 2, 4, 0, 0, 65535, 147)
        for t, datagram in ((10, fused(1, 15.0)), (11, fused(1, 99.0)), (12, fused(1, 15.0, False)),
                            (13, b"\x01\x00\x11\xc5" + bytes(30))):
            capture += struct.pack("<IIII", 1_796_000_400 + t, 0, len(datagram), len(datagram)) + datagram
        (self.root / "2026-11-30/2026-11-30T01Z.pcap").write_bytes(capture)
        (self.root / "manifest.jsonl").write_text(json.dumps(
            {"file": "2026-11-30/2026-11-30T01Z.pcap", "sha256": hashlib.sha256(capture).hexdigest()}) + "\n")
        intervals = Path(self.directory.name) / "intervals.json"
        intervals.write_text(json.dumps({"intervals": [interval("home_empty", 0, 3600)], "walk_ins": []}))
        return intervals

    def test_mmwave_parsing_keeps_only_present_plausible_values(self):
        self.write_inputs()
        files, _ = g2.verified_files(self.root, "manifest.jsonl")
        self.assertEqual(g2.mmwave_breathing(files), [(BASE + 10 * NS, 1, 15.0)])

    def test_truncated_captures_fail_cleanly(self):
        intervals = self.write_inputs()
        capture = self.root / "2026-11-30/2026-11-30T01Z.pcap"
        for raw in (b"", capture.read_bytes()[:-5]):
            capture.write_bytes(raw)
            (self.root / "manifest.jsonl").write_text(json.dumps(
                {"file": "2026-11-30/2026-11-30T01Z.pcap", "sha256": hashlib.sha256(raw).hexdigest()}) + "\n")
            out = Path(self.directory.name)
            self.assertEqual(g2.main(["--data", str(self.root), "--intervals", str(intervals), "--split", "train",
                                      "--report", str(out / "r.md"), "--json", str(out / "r.json")]), 1)

    def test_nearest_rank_p90(self):
        self.assertEqual(g2.nearest_rank(list(range(1, 11)), 0.9), 9)
        self.assertEqual(g2.nearest_rank([5.0], 0.9), 5.0)
        self.assertIsNone(g2.nearest_rank([], 0.9))

    def test_cli_scores_once_and_checks_hashes(self):
        intervals = self.write_inputs()
        out = Path(self.directory.name)
        args = ["--data", str(self.root), "--intervals", str(intervals), "--report", str(out / "r.md"),
                "--json", str(out / "r.json")]
        self.assertEqual(g2.main(args), 1)  # test split needs a freeze commit
        self.assertEqual(g2.main(args + ["--freeze-commit", "abc123"]), 0)
        report = json.loads((out / "r.json").read_text())
        self.assertEqual(report["evidence"], "MEASURED")
        self.assertEqual(report["results"]["g2"], "insufficient data")
        self.assertIn("Freeze commit `abc123`", (out / "r.md").read_text())
        self.assertEqual(g2.main(args + ["--freeze-commit", "abc123"]), 1)  # already scored
        self.assertEqual(g2.main(args + ["--freeze-commit", "def456", "--rescore-reason", "label fix"]), 0)
        history = json.loads((self.root / "scoring/test-scored.json").read_text())
        self.assertEqual([h["rescore_reason"] for h in history], [None, "label fix"])
        self.assertEqual(g2.main(args[:-4] + ["--split", "train", "--report", str(out / "t.md"),
                                              "--json", str(out / "t.json")]), 0)
        (self.root / "outputs/2026-11-30/2026-11-30T01Z.jsonl").write_text("{}\n")
        self.assertEqual(g2.main(args + ["--freeze-commit", "x", "--rescore-reason", "y"]), 1)
        (self.root / "outputs/2026-11-30/2026-11-30T02Z.jsonl").write_text("{}\n")
        with self.assertRaises(g2.ScoreError):
            g2.verified_files(self.root, "outputs-manifest.jsonl")


if __name__ == "__main__":
    unittest.main()

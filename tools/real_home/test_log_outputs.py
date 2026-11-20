"""SYNTHETIC tests for the live-verdict logger. No server, no captured data."""
import hashlib
import json
import tempfile
import unittest
import urllib.error
from pathlib import Path

import log_outputs

HOUR_NS = 1_796_000_400 * 1_000_000_000  # 2026-11-30T01:00Z


def update(tick, presence=True, level="present_still"):
    return {"timestamp": 1_796_000_400.5, "tick": tick, "source": "esp32",
            "classification": {"presence": presence, "motion_level": level, "confidence": 0.8},
            "nodes": [{"node_id": 3, "amplitude": [1.0] * 64, "position": [1, 2, 3],
                       "node_inference": {"classification": level, "confidence": 0.8, "age_ms": 40}}],
            "vital_signs": {"breathing_rate_bpm": 14.5, "breathing_confidence": 0.7, "heart_rate_bpm": 70}}


class LoggerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name) / "home"

    def tearDown(self):
        self.directory.cleanup()

    def run_with(self, responses, clock_values):
        responses, clock = iter(responses), iter(clock_values)

        def get(url, token):
            value = next(responses)
            if isinstance(value, Exception):
                raise value
            return value

        return log_outputs.run("http://127.0.0.1:8080", self.root, get=get, clock=lambda: next(clock),
                               sleep=lambda _: None, max_polls=len(clock_values) // 2)

    def lines(self):
        out = []
        for path in sorted((self.root / "outputs").rglob("*.jsonl")):
            out += [json.loads(line) for line in path.read_text().splitlines()]
        return out

    def test_keeps_scoring_fields_only_and_dedupes_ticks(self):
        links = {"mode": "best_link", "selected": 3, "last_ranking": [{"node": 3}]}
        responses = [update(1), links, update(1), update(2, False, "absent")]
        clocks = [HOUR_NS, HOUR_NS, HOUR_NS + 250_000_000, HOUR_NS + 260_000_000,
                  HOUR_NS + 500_000_000, HOUR_NS + 510_000_000]
        self.assertEqual(self.run_with(responses, clocks), 3)
        lines = self.lines()
        self.assertEqual([("tick" in r, "links" in r) for r in lines], [(True, False), (False, True), (True, False)])
        first = lines[0]
        self.assertEqual(first["nodes"], {"3": {"level": "present_still", "age_ms": 40}})
        self.assertEqual((first["presence"], first["breathing_bpm"]), (True, 14.5))
        self.assertNotIn("amplitude", json.dumps(lines))
        self.assertNotIn("heart_rate", json.dumps(lines))
        self.assertEqual(lines[1]["links"], {"mode": "best_link", "selected": 3})
        self.assertFalse(lines[2]["presence"])

    def test_outages_are_logged_once_and_files_rotate_with_hashes(self):
        responses = [urllib.error.URLError("refused"), urllib.error.URLError("refused"),
                     {"status": "stale", "age_ms": 9000}, {"mode": "fuse_all", "selected": None}, update(5)]
        clocks = [HOUR_NS - 2, HOUR_NS - 2, HOUR_NS - 1, HOUR_NS - 1, HOUR_NS, HOUR_NS,
                  HOUR_NS + 1, HOUR_NS + 1]
        self.run_with(responses, clocks)
        lines = self.lines()
        self.assertEqual([r.get("status") for r in lines], ["unreachable", "stale", None, None])
        self.assertEqual(lines[2]["links"], {"mode": "fuse_all", "selected": None})
        self.assertEqual(lines[3]["tick"], 5)
        manifest = [json.loads(line) for line in (self.root / "outputs-manifest.jsonl").read_text().splitlines()]
        self.assertEqual([m["file"] for m in manifest],
                         ["outputs/2026-11-30/2026-11-30T00Z.jsonl", "outputs/2026-11-30/2026-11-30T01Z.jsonl"])
        for entry in manifest:
            self.assertEqual(hashlib.sha256((self.root / entry["file"]).read_bytes()).hexdigest(), entry["sha256"])

    def test_guards(self):
        with self.assertRaises(log_outputs.OutputError):
            log_outputs.loopback_url("http://192.168.1.5:8080")
        with self.assertRaises(ValueError):
            log_outputs.loopback_url("http://example.com:8080")
        self.assertEqual(log_outputs.loopback_url("http://localhost:8080/"), "http://localhost:8080")
        with self.assertRaises(log_outputs.OutputError):
            log_outputs.private_output(Path(__file__).resolve().parent / "out")


if __name__ == "__main__":
    unittest.main()

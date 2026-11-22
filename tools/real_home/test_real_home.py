"""SYNTHETIC tests for the real-home recorder and labels. No hardware, no captured data."""
import csv
import hashlib
import json
import socket
import struct
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import labels
import record_tee

UTC = timezone.utc
ZONE = ZoneInfo("America/Los_Angeles")


def csi(node, payload=b"\x00" * 16):
    return struct.pack("<IB", record_tee.CSI_MAGIC, node) + payload


def read_pcap(path):
    raw = Path(path).read_bytes()
    magic, major, minor, _, _, snaplen, linktype = struct.unpack_from("<IHHiIII", raw)
    assert (magic, major, minor, snaplen, linktype) == (0xA1B23C4D, 2, 4, 65535, 147)
    offset, out = 24, []
    while offset < len(raw):
        seconds, fraction, captured, original = struct.unpack_from("<IIII", raw, offset)
        out.append((seconds * 1_000_000_000 + fraction, raw[offset + 16:offset + 16 + captured]))
        offset += 16 + captured
    return out


class RecorderTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name) / "capture"

    def tearDown(self):
        self.directory.cleanup()

    def test_output_and_listen_guards(self):
        with self.assertRaises(record_tee.RecorderError):
            record_tee.loopback_address("192.168.1.10:5600")
        with self.assertRaises(record_tee.RecorderError):
            record_tee.loopback_address("127.0.0.1:0")
        self.assertEqual(record_tee.loopback_address("[::1]:5600"), ("::1", 5600))
        with self.assertRaises(record_tee.RecorderError) as caught:
            record_tee.private_output(Path(__file__).resolve().parent / "out")
        self.assertEqual(str(caught.exception), "output_must_be_outside_git")

    def test_hourly_files_are_exact_and_closed_with_a_manifest(self):
        hour = 1_796_000_400 * 1_000_000_000  # an exact UTC hour
        frames = [(hour - 2, csi(3)), (hour - 1, b"\x02\x00\x11\xc5" + bytes(28)), (hour, csi(3)),
                  (hour + 5, csi(4, b"x"))]
        clock = iter([t for t, _ in frames])
        datagrams = iter([d for _, d in frames])

        class FakeSocket:
            def recv(self, size):
                return next(datagrams)

        summary = record_tee.record(FakeSocket(), self.root, clock=lambda: next(clock), max_datagrams=4,
                                    min_free_bytes=0, log=lambda *_: None)
        self.assertEqual(summary["datagrams"], 4)
        manifest = [json.loads(line) for line in (self.root / "manifest.jsonl").read_text().splitlines()]
        self.assertEqual([m["datagrams"] for m in manifest], [2, 2])
        self.assertEqual(manifest[0]["by_magic"], {"0xC5110001": 1, "0xC5110002": 1})
        self.assertEqual(manifest[1]["csi_by_node"], {"3": 1, "4": 1})
        for entry, expected in zip(manifest, (frames[:2], frames[2:])):
            path = self.root / entry["file"]
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), entry["sha256"])
            self.assertEqual(read_pcap(path), expected)
        self.assertEqual(manifest[1]["file"], "2026-11-30/2026-11-30T01Z.pcap")
        self.assertFalse(list(self.root.rglob("*.partial")))

    def test_restart_in_the_same_hour_never_overwrites(self):
        hour = 1_796_000_400 * 1_000_000_000
        for _ in range(2):
            stream = iter([csi(1)])

            class FakeSocket:
                def recv(self, size, stream=stream):
                    return next(stream)

            record_tee.record(FakeSocket(), self.root, clock=lambda: hour + 1, max_datagrams=1,
                              min_free_bytes=0, log=lambda *_: None)
        names = sorted(p.name for p in self.root.rglob("*.pcap"))
        self.assertEqual(names, ["2026-11-30T01Z-r1.pcap", "2026-11-30T01Z.pcap"])

    def test_a_crash_is_recorded_in_the_manifest(self):
        stream = iter([csi(1)])

        class FakeSocket:
            def recv(self, size):
                try:
                    return next(stream)
                except StopIteration:
                    raise OSError("socket closed") from None

        with self.assertRaises(OSError):
            record_tee.record(FakeSocket(), self.root, clock=lambda: 10**18, min_free_bytes=0, log=lambda *_: None)
        manifest = [json.loads(line) for line in (self.root / "manifest.jsonl").read_text().splitlines()]
        self.assertEqual(manifest[0]["note"], "error:OSError")

    def test_clock_jump_is_logged(self):
        times = iter([10**18, 10**18 + 5 * 10**9])
        monotonic = iter([0, 0, 1, 1, 2, 2, 3, 3, 4])
        stream = iter([csi(1), csi(1)])

        class FakeSocket:
            def recv(self, size):
                return next(stream)

        record_tee.record(FakeSocket(), self.root, clock=lambda: next(times), monotonic=lambda: next(monotonic),
                          max_datagrams=2, min_free_bytes=0, log=lambda *_: None)
        events = [json.loads(line) for line in (self.root / "events.jsonl").read_text().splitlines()]
        self.assertEqual([e["event"] for e in events], ["clock_jump"])

    def test_real_loopback_socket(self):
        receiver = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(2)
        port = receiver.getsockname()[1]
        result = {}
        thread = threading.Thread(target=lambda: result.update(record_tee.record(
            receiver, self.root, max_datagrams=3, min_free_bytes=0, log=lambda *_: None)))
        thread.start()
        sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        for node in (1, 2, 3):
            sender.sendto(csi(node), ("127.0.0.1", port))
        thread.join(5)
        sender.close()
        receiver.close()
        self.assertEqual(result["datagrams"], 3)
        packets = [d for path in sorted(self.root.rglob("*.pcap")) for _, d in read_pcap(path)]
        self.assertEqual(packets, [csi(1), csi(2), csi(3)])


def at(day, hour, minute=0, second=0):
    """UTC time for local wall time on 2026-12-0<day> in Los Angeles (UTC-8)."""
    local = datetime(2026, 12, day, hour, minute, second, tzinfo=ZONE)
    return labels.format_time(local)


def write(path, rows):
    with open(path, "w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=labels.FIELDS, lineterminator="\n")
        writer.writeheader()
        for time_utc, event, *rest in rows:
            spot, source = (rest + ["", ""])[:2]
            writer.writerow({"time_utc": time_utc, "room": "study", "event": event, "spot": spot,
                             "source": source, "note": ""})


class LabelTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "labels.csv"

    def tearDown(self):
        self.directory.cleanup()

    def summary(self, rows, spots=None):
        write(self.path, rows)
        return labels.summarize(labels.read_labels(self.path), ZONE, spots)

    def test_guards_days_and_split(self):
        rows = []
        for day in range(1, 6):
            rows += [(at(day, 9), "home_empty_start"), (at(day, 17), "home_empty_end"),
                     (at(day, 18), "still_start", "S1"), (at(day, 18, 30), "still_end", "S1"),
                     (at(day, 19), "room_empty_start"), (at(day, 19, 4), "room_empty_end"),
                     (at(day, 19, 5), "walk_in", "", "door")]
        rows.sort()
        report, scoring = self.summary(rows)
        day1 = report["days"]["1"]
        self.assertAlmostEqual(day1["empty_home_hours"], 8 - 150 / 3600, places=3)
        self.assertAlmostEqual(day1["still_minutes_in_area"], 29.0)
        self.assertEqual(day1["walk_ins"], 0)  # walk_in has no room_empty_end at the same time
        self.assertEqual(day1["walk_ins_without_empty_lead"], 1)
        self.assertEqual([report["days"][str(d)]["split"] for d in range(1, 6)],
                         ["train", "train", "train", "test", "test"])
        self.assertFalse(report["minimum_test_data_met"])
        self.assertIn("walk_ins", report["missing"])
        self.assertEqual({i["split"] for i in scoring["intervals"] if i["day"] == 5}, {"test"})

    def test_mark_closes_room_empty_and_walk_in_counts(self):
        labels.append_row(self.path, {"time_utc": at(1, 19), "room": "study", "event": "room_empty_start",
                                      "spot": "", "source": "", "note": ""})
        written = labels.append_row(self.path, {"time_utc": at(1, 19, 5), "room": "study", "event": "walk_in",
                                                "spot": "", "source": "button", "note": ""})
        self.assertEqual([r["event"] for r in written], ["room_empty_end", "walk_in"])
        report, scoring = labels.summarize(labels.read_labels(self.path), ZONE)
        self.assertEqual(report["days"]["1"]["walk_ins"], 1)
        self.assertEqual(scoring["walk_ins"][0]["prior_room_empty_seconds"], 300.0)
        with self.assertRaises(labels.LabelError):
            labels.append_row(self.path, {"time_utc": at(1, 18), "room": "study", "event": "still_start",
                                          "spot": "S1", "source": "", "note": ""})

    def test_night_stays_in_one_day_and_dst_is_handled(self):
        report, _ = self.summary([(at(1, 23), "sleep_start"), (at(2, 3, 59), "sleep_end")])
        self.assertEqual(list(report["days"]), ["1"])
        self.assertAlmostEqual(report["days"]["1"]["sleep_hours"], 4 + 59 / 60 - 1 / 60, places=3)
        # 2026-11-01 is the US fall-back day: 25 local hours from 04:00 to 04:00.
        start = labels.format_time(datetime(2026, 10, 31, 23, tzinfo=ZONE))
        end = labels.format_time(datetime(2026, 11, 1, 3, 30, tzinfo=ZONE))
        report, _ = self.summary([(start, "sleep_start"), (end, "sleep_end")])
        self.assertAlmostEqual(report["days"]["1"]["sleep_hours"], 5.5 - 1 / 60, places=3)

    def test_excluded_time_is_removed_and_heavy_days_dropped(self):
        rows = [(at(1, 8), "exclude_start"), (at(1, 9), "home_empty_start"), (at(1, 10), "exclude_end"),
                (at(1, 17), "home_empty_end"), (at(2, 5), "exclude_start"), (at(2, 12), "exclude_end")]
        report, _ = self.summary(rows)
        self.assertAlmostEqual(report["days"]["1"]["empty_home_hours"], 7 - 30 / 3600, places=3)
        self.assertEqual(report["days"]["2"]["split"], "dropped")

    def test_spots_outside_the_area(self):
        rows = [(at(1, 18), "still_start", "S2"), (at(1, 18, 20), "still_end", "S2"),
                (at(1, 19), "still_start", "S3-out"), (at(1, 19, 20), "still_end", "S3-out")]
        report, scoring = self.summary(rows, spots={"S1"})
        self.assertEqual([i["in_area"] for i in scoring["intervals"]], [False, False])
        self.assertEqual((report["days"]["1"]["still_minutes_in_area"], report["days"]["1"]["still_minutes_outside"]),
                         (0.0, 38.0))
        report, _ = self.summary(rows)
        self.assertEqual(report["days"]["1"]["still_minutes_in_area"], 19.0)

    def test_pets_alone_is_guarded_and_kept_apart_from_empty_home(self):
        rows = [(at(1, 9), "home_empty_start"), (at(1, 12), "home_empty_end"),
                (at(1, 13), "pets_alone_start"), (at(1, 17), "pets_alone_end")]
        report, scoring = self.summary(rows)
        day1 = report["days"]["1"]
        self.assertAlmostEqual(day1["empty_home_hours"], 3 - 150 / 3600, places=3)
        self.assertAlmostEqual(day1["pets_alone_hours"], 4 - 150 / 3600, places=3)
        self.assertEqual(sorted(i["state"] for i in scoring["intervals"]), ["home_empty", "pets_alone"])

    def test_rejects_bad_files(self):
        cases = [
            [(at(1, 9), "home_empty_start"), (at(1, 10), "still_start", "S1"), (at(1, 11), "still_end", "S1"),
             (at(1, 12), "home_empty_end")],                                   # overlap
            [(at(1, 9), "home_empty_end")],                                    # end without start
            [(at(1, 9), "home_empty_start")],                                  # never closed
            [(at(1, 9), "still_start")],                                       # still without spot
            [(at(1, 9), "walk_in")],                                           # walk-in without source
            [(at(1, 10), "home_empty_start"), (at(1, 9), "home_empty_end")],   # out of order
            [(at(1, 9), "home_empty_start"), (at(1, 9, 30), "walk_in", "", "door"),
             (at(1, 10), "home_empty_end")],                                    # walk-in while home empty
            [(at(1, 9), "pets_alone_start"), (at(1, 9, 30), "walk_in", "", "door"),
             (at(1, 10), "pets_alone_end")],                                    # walk-in while pets alone
            [(at(1, 9), "home_empty_start"), (at(1, 10), "pets_alone_start"),
             (at(1, 11), "pets_alone_end"), (at(1, 12), "home_empty_end")],    # pets loose and confined at once
        ]
        for rows in cases:
            with self.subTest(rows=rows), self.assertRaises(labels.LabelError):
                self.summary(rows)
        self.path.write_text("time,room,event\n")
        with self.assertRaises(labels.LabelError):
            labels.read_labels(self.path)
        with self.assertRaises(labels.LabelError):
            labels.parse_time("2026-12-01T10:00:00+00:00")

    def test_minimum_test_data(self):
        rows = []
        for day in range(1, 7):
            rows += [(at(day, 6), "home_empty_start"), (at(day, 22), "home_empty_end"),
                     (at(day, 22, 10), "still_start", "S1"), (at(day, 23, 59), "still_end", "S1")]
        for n in range(30):
            start = datetime(2026, 12, 7, 6, tzinfo=ZONE) + timedelta(minutes=5 * n)
            rows += [(labels.format_time(start), "room_empty_start"),
                     (labels.format_time(start + timedelta(minutes=3)), "room_empty_end"),
                     (labels.format_time(start + timedelta(minutes=3)), "walk_in", "", "door")]
        report, scoring = self.summary(sorted(rows))
        self.assertTrue(report["minimum_test_data_met"], report["missing"])
        self.assertEqual(len([w for w in scoring["walk_ins"] if w["split"] == "test"]), 30)

    def test_cli(self):
        self.assertEqual(labels.main(["mark", "room_empty_start", "--labels", str(self.path), "--at", at(1, 9)]), 0)
        self.assertEqual(labels.main(["mark", "walk_in", "--labels", str(self.path), "--source", "door",
                                      "--at", at(1, 9, 3)]), 0)
        out = Path(self.directory.name) / "intervals.json"
        self.assertEqual(labels.main(["check", "--labels", str(self.path), "--timezone", "America/Los_Angeles",
                                      "--intervals", str(out)]), 3)
        self.assertEqual(json.loads(out.read_text())["walk_ins"][0]["source"], "door")
        self.assertEqual(labels.main(["mark", "still_start", "--labels", str(self.path), "--at", at(1, 8)]), 1)
        inside = Path(__file__).resolve().parent / "labels.csv"
        self.assertEqual(labels.main(["mark", "room_empty_start", "--labels", str(inside)]), 1)
        self.assertFalse(inside.exists())


if __name__ == "__main__":
    unittest.main()

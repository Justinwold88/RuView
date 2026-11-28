"""SYNTHETIC tests for the mmWave reference recorder. No kit, no captured data.

A fake ESPHome device on loopback speaks the plaintext native API, so the
recorder's handshake, entity choice, filtering and read-only behaviour are
checked end to end.
"""
import hashlib
import json
import socket
import struct
import tempfile
import threading
import unittest
from pathlib import Path

import record_mmwave as rm


def entity(msg_type, key, object_id, name, unit=""):
    payload = rm.pb_string(1, object_id) + rm.encode_varint(2 << 3 | 5) + struct.pack("<I", key) + rm.pb_string(3, name)
    if unit:
        payload += rm.pb_string(6, unit)
    return rm.encode_frame(msg_type, payload)


def sensor_state(key, value, missing=False):
    payload = rm.encode_varint(1 << 3 | 5) + struct.pack("<I", key) + rm.encode_varint(2 << 3 | 5) + struct.pack("<f", value)
    if missing:
        payload += rm.pb_uint(3, 1)
    return rm.encode_frame(rm.STATE_SENSOR, payload)


def binary_state(key, value):
    payload = rm.encode_varint(1 << 3 | 5) + struct.pack("<I", key) + rm.pb_uint(2, 1 if value else 0)
    return rm.encode_frame(rm.STATE_BINARY, payload)


# The MR60BHA2 kit's ESPHome entities, plus two the recorder must ignore.
KIT_ENTITIES = (entity(rm.LIST_BINARY, 1, "person_information", "Person Information")
                + entity(rm.LIST_SENSOR, 2, "real_time_respiratory_rate", "Real-time respiratory rate", "bpm")
                + entity(rm.LIST_SENSOR, 3, "real_time_heart_rate", "Real-time heart rate", "bpm")
                + entity(rm.LIST_SENSOR, 4, "distance_to_detection_object", "Distance to detection object", "cm")
                + entity(rm.LIST_SENSOR, 5, "illuminance", "Seeed MR60BHA2 Illuminance", "lx"))


class FakeKit:
    """A one-connection ESPHome device. `script` maps the request it waits for to what it sends."""

    def __init__(self, mode="normal"):
        self.server = socket.create_server(("127.0.0.1", 0))
        self.port = self.server.getsockname()[1]
        self.mode, self.received = mode, []
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        conn, _ = self.server.accept()
        try:
            self.converse(conn)
        except OSError:
            pass  # the recorder hung up (password or encryption refused)
        finally:
            conn.close()

    def converse(self, conn):
        with conn:
            conn.settimeout(5)
            buf = b""
            if self.mode == "encrypted":
                conn.sendall(b"\x01\x00\x00")
            sent_states = False
            while True:
                try:
                    chunk = conn.recv(65536)
                except OSError:
                    return
                if not chunk:
                    return
                buf += chunk
                frames, buf = rm.split_frames(buf)
                for msg_type, _ in frames:
                    self.received.append(msg_type)
                    if msg_type == rm.HELLO_REQ:
                        conn.sendall(rm.encode_frame(rm.HELLO_RES, rm.pb_uint(1, 1) + rm.pb_uint(2, 10)))
                    elif msg_type == rm.CONNECT_REQ:
                        conn.sendall(rm.encode_frame(rm.CONNECT_RES, rm.pb_uint(1, 1) if self.mode == "password" else b""))
                    elif msg_type == rm.DEVICE_INFO_REQ:
                        conn.sendall(rm.encode_frame(rm.DEVICE_INFO_RES, rm.pb_string(2, "mr60bha2-kit")
                                                     + rm.pb_string(4, "2026.10.1") + rm.pb_string(6, "esp32c6")))
                    elif msg_type == rm.LIST_REQ:
                        conn.sendall(KIT_ENTITIES + rm.encode_frame(rm.LIST_DONE))
                    elif msg_type == rm.SUBSCRIBE_STATES and not sent_states:
                        sent_states = True
                        conn.sendall(binary_state(1, True) + sensor_state(2, 14.5) + sensor_state(3, 72.0)
                                     + sensor_state(4, 120.0) + sensor_state(5, 300.0) + sensor_state(2, 0.0, missing=True)
                                     + sensor_state(2, float("nan")) + rm.encode_frame(rm.PING_REQ))
                    elif msg_type == rm.PING_RES:
                        conn.sendall(sensor_state(2, 15.25) + binary_state(1, False))
                        conn.shutdown(socket.SHUT_WR)

    def close(self):
        self.server.close()
        self.thread.join(timeout=5)


class FramingTests(unittest.TestCase):
    def test_frames_round_trip_and_wait_for_more_bytes(self):
        frame = rm.encode_frame(rm.HELLO_RES, rm.pb_string(3, "x" * 200))
        self.assertEqual(rm.split_frames(frame[:-1]), ([], frame[:-1]))
        frames, rest = rm.split_frames(frame + frame[:3])
        self.assertEqual((len(frames), rest), (1, frame[:3]))
        self.assertEqual(rm.field_str(rm.decode_fields(frames[0][1]), 3), "x" * 200)

    def test_encrypted_and_garbled_streams_are_refused(self):
        with self.assertRaises(rm.SourceError):
            rm.split_frames(b"\x01\x05\x00")
        with self.assertRaises(rm.OutputError):
            rm.split_frames(b"\x02\x00\x00")


class AddressTests(unittest.TestCase):
    def test_only_private_link_local_or_loopback_hosts(self):
        for ok in ("192.168.1.40", "10.0.0.7", "172.20.1.2", "169.254.3.4", "100.100.1.1", "::1", "fd00::5", "fe80::1"):
            self.assertTrue(rm.private_address(ok), ok)
        for bad in ("8.8.8.8", "172.32.0.1", "2606:4700::1111", "::ffff:8.8.8.8"):
            self.assertFalse(rm.private_address(bad), bad)
        self.assertEqual(rm.parse_target("192.168.1.40"), ("192.168.1.40", 6053))
        self.assertEqual(rm.parse_target("[fe80::1]:6054"), ("fe80::1", 6054))
        with self.assertRaises(rm.SourceError):
            rm.parse_target("8.8.8.8:6053")
        public = lambda *a, **k: [(None, None, None, None, ("93.184.216.34", 6053))]  # noqa: E731
        with self.assertRaises(rm.SourceError):
            rm.parse_target("kit.example", resolve=public)
        private = lambda *a, **k: [(None, None, None, None, ("192.168.1.41", 6053))]  # noqa: E731
        self.assertEqual(rm.parse_target("mr60bha2.local", resolve=private), ("192.168.1.41", 6053))
        for bad in ("kit:port", "kit:0", "[::1"):
            with self.assertRaises(rm.OutputError):
                rm.parse_target(bad, resolve=private)


class EsphomeTests(unittest.TestCase):
    def record(self, mode):
        kit = FakeKit(mode)
        records, ticks = [], iter(range(1, 10**6))
        try:
            rm.run_esphome(("127.0.0.1", kit.port), records.append, clock=lambda: next(ticks) * 1_000_000,
                           sleep=lambda _: None, max_sessions=1)
        finally:
            kit.close()
        return records, kit.received

    def test_kit_session_keeps_presence_and_breathing_only(self):
        records, received = self.record("normal")
        status = records[0]
        self.assertEqual(status["status"], "connected")
        self.assertEqual(status["device"], {"name": "mr60bha2-kit", "esphome_version": "2026.10.1", "model": "esp32c6"})
        self.assertEqual(status["entities"], {"person_information": "presence",
                                              "real_time_respiratory_rate": "breathing_bpm"})
        values = [(r["field"], r["value"]) for r in records if "field" in r]
        # Heart rate, distance, illuminance, a missing state and NaN are not kept.
        self.assertEqual(values, [("presence", True), ("breathing_bpm", 14.5), ("breathing_bpm", 15.25),
                                  ("presence", False)])
        self.assertEqual(records[-1]["status"], "disconnected")
        # Read-only: only the handshake, the list, the subscription, ping replies and a goodbye.
        allowed = {rm.HELLO_REQ, rm.CONNECT_REQ, rm.DEVICE_INFO_REQ, rm.LIST_REQ, rm.SUBSCRIBE_STATES,
                   rm.PING_RES, rm.DISCONNECT_REQ}
        self.assertTrue(set(received) <= allowed, received)
        self.assertIn(rm.PING_RES, received)

    def test_password_and_encryption_stop_the_recorder(self):
        for mode in ("password", "encrypted"):
            with self.assertRaises(rm.SourceError) as caught:
                self.record(mode)
            self.assertIn(mode if mode == "encrypted" else "password", str(caught.exception))


class LogTests(unittest.TestCase):
    LOG = [
        "\x1b[0;36m[D][binary_sensor:036]: 'Person Information': Sending state ON\x1b[0m",
        "[D][sensor:094]: 'Real-time respiratory rate': Sending state 13.00000 bpm with 0 decimals of accuracy",
        "[D][sensor:094]: 'Real-time heart rate': Sending state 71.00000 bpm with 0 decimals of accuracy",
        "[D][sensor:094]: 'Distance to detection object': Sending state 118.00000 cm with 0 decimals of accuracy",
        "[D][sensor:094]: 'Real-time respiratory rate': Sending state nan bpm with 0 decimals of accuracy",
        "[I][app:102]: ESPHome version 2026.10.1 compiled on Nov 1 2026",
        "[D][binary_sensor:036]: 'Person Information': Sending state OFF",
    ]

    def test_log_lines(self):
        parsed = [rm.parse_log_line(line) for line in self.LOG]
        self.assertEqual([p for p in parsed if p], [("presence", True), ("breathing_bpm", 13.0), ("presence", False)])

    def test_replay_writes_hourly_files_with_a_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "esphome.log"
            log.write_text("\n".join(self.LOG) + "\n")
            out = Path(directory) / "study"
            self.assertEqual(rm.main(["--replay", str(log), "--output", str(out)]), 0)
            manifest = [json.loads(line) for line in (out / "mmwave-manifest.jsonl").read_text().splitlines()]
            self.assertEqual(len(manifest), 1)
            path = out / manifest[0]["file"]
            self.assertTrue(manifest[0]["file"].startswith("mmwave/"))
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), manifest[0]["sha256"])
            lines = [json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual([line.get("field") for line in lines], [None, "presence", "breathing_bpm", "presence", None])

    def test_output_inside_a_git_checkout_is_refused(self):
        here = Path(__file__).resolve().parent
        self.assertEqual(rm.main(["--replay", __file__, "--output", str(here / "study-out")]), 1)
        self.assertFalse((here / "study-out").exists())


if __name__ == "__main__":
    unittest.main()

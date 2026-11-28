#!/usr/bin/env python3
"""Record the mmWave reference kit's readings next to the Wi-Fi capture.

Real-home dataset protocol (docs/validation/real-home-dataset-protocol.md),
issue #51, option 1 (chosen 2026-11-27): the Seeed MR60BHA2 kit stays as sold,
running ESPHome, and this tool logs what it reports on the host clock, the
same clock as the tee capture, the output log and the labels.

Python standard library only (pyserial only for --serial). Sources:

- ``--esphome HOST[:PORT]``: the kit's ESPHome native API (TCP 6053) on a
  private, link-local or loopback address. Read-only: it sends only the
  handshake, the entity list and state subscription requests, and ping
  replies, never a command. The plaintext API only; a kit that requires API
  encryption or a password is refused with a remedy. Reconnects with backoff.
- ``--serial PORT``: the kit's USB log (ESPHome ``Sending state`` lines).
- ``--replay FILE``: an ESPHome log file, for checks.

Only what scoring needs is kept: whether the radar reports a person, and its
breathing rate. Heart rate, distance and target positions are not logged.
Lines go to <output>/mmwave/<YYYY-MM-DD>/<YYYY-MM-DDTHH>Z.jsonl, and each
closed file's SHA-256 to <output>/mmwave-manifest.jsonl::

    {"t_ns": ..., "field": "presence", "value": true}
    {"t_ns": ..., "field": "breathing_bpm", "value": 14.2}
    {"t_ns": ..., "status": "connected", "device": {...}, "entities": {...}}
    {"t_ns": ..., "status": "alive"}            # every 30 s while connected
    {"t_ns": ..., "status": "disconnected", "reason": "..."}

ESPHome reports a value when it changes, and sends every current value when a
client subscribes, so a value holds until the next one or a disconnect.

The kit is a reference, not ground truth: its accuracy is CLAIMED by its maker.
"""
from __future__ import annotations

import argparse
import ipaddress
import json
import re
import select
import signal
import socket
import struct
import sys
import time
from pathlib import Path

from log_outputs import HourlyJsonl, OutputError, private_output, require

DEFAULT_PORT = 6053
ALIVE_NS = 30 * 1_000_000_000
SILENT_LIMIT_S = 90.0
MAX_BUFFER = 1024 * 1024
BACKOFF_S = (1, 2, 5, 10, 30, 60)

# Entity roles, matched on name + object id as in harness/ruview/src/devices/esphome.js.
ROLES = (
    ("presence", "binary", re.compile(r"person|presence|occup|motion|human", re.IGNORECASE)),
    ("breathing_bpm", "sensor", re.compile(r"respir|breath", re.IGNORECASE)),
)

# ESPHome native API message types (api.proto).
HELLO_REQ, HELLO_RES, CONNECT_REQ, CONNECT_RES = 1, 2, 3, 4
DISCONNECT_REQ, DISCONNECT_RES, PING_REQ, PING_RES = 5, 6, 7, 8
DEVICE_INFO_REQ, DEVICE_INFO_RES, LIST_REQ = 9, 10, 11
LIST_BINARY, LIST_SENSOR, LIST_DONE, SUBSCRIBE_STATES = 12, 16, 19, 20
STATE_BINARY, STATE_SENSOR = 21, 25


class SourceError(OutputError):
    """A problem that reconnecting won't fix (encrypted API, password, address)."""


# --- protobuf (proto3) and plaintext framing -------------------------------------
def encode_varint(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def read_varint(buf, offset):
    result = shift = 0
    while True:
        if offset >= len(buf):
            return None
        byte = buf[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, offset
        shift += 7
        require(shift <= 63, "esphome_protocol_error:varint_too_long")


def decode_fields(payload):
    """{field_number: [(wire_type, value)]} for wire types 0, 1, 2 and 5."""
    fields, offset = {}, 0
    while offset < len(payload):
        tag = read_varint(payload, offset)
        require(tag is not None, "esphome_protocol_error:truncated_tag")
        key, offset = tag
        number, wire = key >> 3, key & 7
        if wire == 0:
            item = read_varint(payload, offset)
            require(item is not None, "esphome_protocol_error:truncated_varint")
            value, offset = item
        elif wire == 2:
            item = read_varint(payload, offset)
            require(item is not None and item[1] + item[0] <= len(payload), "esphome_protocol_error:truncated_bytes")
            value, offset = bytes(payload[item[1]:item[1] + item[0]]), item[1] + item[0]
        elif wire in (1, 5):
            size = 8 if wire == 1 else 4
            require(offset + size <= len(payload), "esphome_protocol_error:truncated_fixed")
            value, offset = bytes(payload[offset:offset + size]), offset + size
        else:
            raise OutputError(f"esphome_protocol_error:wire_type_{wire}")
        fields.setdefault(number, []).append((wire, value))
    return fields


def field_str(fields, number):
    item = fields.get(number, [(None, None)])[0]
    return item[1].decode("utf-8", "replace") if item[0] == 2 else ""


def field_key(fields, number):
    item = fields.get(number, [(None, None)])[0]
    return struct.unpack("<I", item[1])[0] if item[0] == 5 else None


def field_bool(fields, number):
    item = fields.get(number, [(None, None)])[0]
    return item[0] == 0 and item[1] != 0


def field_float(fields, number):
    item = fields.get(number, [(None, None)])[0]
    return struct.unpack("<f", item[1])[0] if item[0] == 5 else 0.0  # proto3: absent float is 0


def pb_string(number, text):
    data = text.encode()
    return encode_varint(number << 3 | 2) + encode_varint(len(data)) + data


def pb_uint(number, value):
    return encode_varint(number << 3) + encode_varint(value)


def encode_frame(msg_type, payload=b""):
    """Plaintext frame: 0x00, varint(length), varint(type), payload."""
    return b"\x00" + encode_varint(len(payload)) + encode_varint(msg_type) + payload


def split_frames(buf):
    """Complete frames off the front of buf; returns (frames, rest)."""
    frames, offset = [], 0
    while offset < len(buf):
        if buf[offset] == 0x01:
            raise SourceError("esphome_encrypted")
        require(buf[offset] == 0x00, "esphome_protocol_error:bad_frame_indicator")
        length = read_varint(buf, offset + 1)
        if length is None:
            break
        kind = read_varint(buf, length[1])
        if kind is None or kind[1] + length[0] > len(buf):
            break
        frames.append((kind[0], bytes(buf[kind[1]:kind[1] + length[0]])))
        offset = kind[1] + length[0]
    return frames, buf[offset:]


# --- address policy ----------------------------------------------------------------
def private_address(text):
    address = ipaddress.ip_address(text)
    if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
        address = address.ipv4_mapped
    if address.is_loopback or address.is_link_local:
        return True
    if isinstance(address, ipaddress.IPv4Address):
        return any(address in ipaddress.ip_network(n) for n in
                   ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10"))
    return address in ipaddress.ip_network("fc00::/7")


HOST_RE = re.compile(r"^(?=.{1,253}$)[A-Za-z0-9](?:[A-Za-z0-9-]{0,62})(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,62}))*\.?$")


def parse_target(value, resolve=socket.getaddrinfo):
    """HOST[:PORT] → (address, port). Every address the name resolves to must be private."""
    host, port = value, DEFAULT_PORT
    if value.startswith("["):
        require("]" in value, "invalid_host")
        host, rest = value[1:].split("]", 1)
        if rest:
            require(rest.startswith(":") and rest[1:].isdigit(), "invalid_port")
            port = int(rest[1:])
    elif value.count(":") == 1:
        host, port_text = value.split(":")
        require(port_text.isdigit(), "invalid_port")
        port = int(port_text)
    require(1 <= port <= 65535, "invalid_port")
    try:
        ipaddress.ip_address(host)
        addresses = [host]
    except ValueError:
        require(bool(HOST_RE.match(host)), "invalid_host")
        try:
            addresses = sorted({info[4][0] for info in resolve(host, port, proto=socket.IPPROTO_TCP)})
        except OSError as error:
            raise SourceError(f"host_unresolved:{host}") from error
    if not addresses or not all(private_address(a) for a in addresses):
        raise SourceError("host_must_be_private_link_local_or_loopback")
    return addresses[0], port


def role_of(kind, name, object_id):
    for field, role_kind, pattern in ROLES:
        if kind == role_kind and pattern.search(f"{name} {object_id}"):
            return field
    return None


def keep(field, value):
    """The value to log, or None to drop it (out-of-range breathing, NaN)."""
    if field == "presence":
        return bool(value)
    if isinstance(value, float) and value != value:
        return None
    return round(float(value), 2) if 0.0 <= float(value) <= 60.0 else None


# --- ESPHome native API source -----------------------------------------------------
class EsphomeSession:
    """One connection: handshake, entity list, state stream. Calls emit(record)."""

    def __init__(self, sock, emit, clock):
        self.sock, self.emit, self.clock = sock, emit, clock
        self.entities, self.chosen, self.device, self.hello = {}, {}, {}, False
        self.last_data = time.monotonic()

    def start(self):
        hello = pb_string(1, "ruview-record-mmwave") + pb_uint(2, 1) + pb_uint(3, 10)
        self.sock.sendall(encode_frame(HELLO_REQ, hello) + encode_frame(CONNECT_REQ)
                          + encode_frame(DEVICE_INFO_REQ) + encode_frame(LIST_REQ))

    def handle(self, msg_type, payload):
        fields = decode_fields(payload)
        if msg_type == HELLO_RES:
            self.hello = True
        elif msg_type == CONNECT_RES and field_bool(fields, 1):
            raise SourceError("esphome_password_required")
        elif msg_type == DEVICE_INFO_RES:
            self.device = {k: v for k, v in (("name", field_str(fields, 2)), ("esphome_version", field_str(fields, 4)),
                                              ("model", field_str(fields, 6)), ("project", field_str(fields, 8)),
                                              ("project_version", field_str(fields, 9))) if v}
        elif msg_type == PING_REQ:
            self.sock.sendall(encode_frame(PING_RES))
        elif msg_type in (LIST_BINARY, LIST_SENSOR):
            key = field_key(fields, 2)
            kind = "binary" if msg_type == LIST_BINARY else "sensor"
            role = role_of(kind, field_str(fields, 3), field_str(fields, 1))
            if key is not None and role and role not in self.chosen.values():
                self.entities[key] = role
                self.chosen[field_str(fields, 1) or field_str(fields, 3)] = role
        elif msg_type == LIST_DONE:
            require(self.chosen, "esphome_no_radar_entities")
            self.emit({"t_ns": self.clock(), "status": "connected", "device": self.device,
                       "entities": dict(sorted(self.chosen.items()))})
            self.sock.sendall(encode_frame(SUBSCRIBE_STATES))
        elif msg_type in (STATE_BINARY, STATE_SENSOR):
            role = self.entities.get(field_key(fields, 1))
            if role and not field_bool(fields, 3):
                value = keep(role, field_bool(fields, 2) if msg_type == STATE_BINARY else field_float(fields, 2))
                if value is not None:
                    self.emit({"t_ns": self.clock(), "field": role, "value": value})


def run_esphome(target, emit, *, clock=time.time_ns, stop=lambda: False, connect=socket.create_connection,
                sleep=time.sleep, max_sessions=None):
    """Stream until stop(); reconnect with backoff. Raises SourceError for problems a retry won't fix."""
    address, port = target
    sessions = failures = 0
    while not stop() and (max_sessions is None or sessions < max_sessions):
        sessions += 1
        reason = "closed"
        try:
            with connect((address, port), timeout=5.0) as sock:
                sock.settimeout(None)
                session = EsphomeSession(sock, emit, clock)
                session.start()
                buf, next_alive = b"", clock() + ALIVE_NS
                while not stop():
                    ready, _, _ = select.select([sock], [], [], 1.0)
                    if ready:
                        chunk = sock.recv(65536)
                        if not chunk:
                            break
                        session.last_data = time.monotonic()
                        buf += chunk
                        require(len(buf) <= MAX_BUFFER, "esphome_protocol_error:buffer_overflow")
                        frames, buf = split_frames(buf)
                        for msg_type, payload in frames:
                            session.handle(msg_type, payload)
                        if session.chosen:
                            failures = 0
                    elif time.monotonic() - session.last_data > SILENT_LIMIT_S:
                        reason = "silent"
                        break
                    if session.chosen and clock() >= next_alive:
                        emit({"t_ns": clock(), "status": "alive"})
                        next_alive = clock() + ALIVE_NS
                else:
                    reason = "stopped"
                try:
                    sock.sendall(encode_frame(DISCONNECT_REQ))
                except OSError:
                    pass
        except SourceError:
            raise
        except (OSError, OutputError) as error:
            reason = str(error) if isinstance(error, OutputError) else f"unreachable:{type(error).__name__}"
        emit({"t_ns": clock(), "status": "disconnected", "reason": reason})
        if stop() or reason == "stopped":
            break
        sleep(BACKOFF_S[min(failures, len(BACKOFF_S) - 1)])
        failures += 1
    return sessions


# --- ESPHome log source (USB serial or a saved log) -----------------------------
LOG_RE = re.compile(r"'(?P<name>[^']{1,80})': Sending state (?P<value>\S+)")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def parse_log_line(line):
    """(field, value) for a radar line of an ESPHome log, else None."""
    match = LOG_RE.search(ANSI_RE.sub("", line))
    if not match:
        return None
    raw = match.group("value")
    if raw.upper() in ("ON", "OFF"):
        field = role_of("binary", match.group("name"), "")
        return (field, raw.upper() == "ON") if field else None
    field = role_of("sensor", match.group("name"), "")
    try:
        value = keep(field, float(raw)) if field else None
    except ValueError:
        return None
    return None if value is None else (field, value)


def run_lines(lines, emit, *, clock=time.time_ns, stop=lambda: False, label="log"):
    emit({"t_ns": clock(), "status": "connected", "device": {"source": label}, "entities": {}})
    count = 0
    for line in lines:
        if stop():
            break
        parsed = parse_log_line(line)
        if parsed:
            emit({"t_ns": clock(), "field": parsed[0], "value": parsed[1]})
            count += 1
    emit({"t_ns": clock(), "status": "disconnected", "reason": "end_of_log"})
    return count


def serial_lines(port, baud):
    try:
        import serial  # pyserial, only for --serial
    except ImportError as error:
        raise SourceError("pyserial_required_for_serial: pip install pyserial") from error
    with serial.Serial(port, baud, timeout=1) as link:
        while True:
            raw = link.readline()
            if raw:
                yield raw.decode("utf-8", "replace")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--esphome", metavar="HOST[:PORT]", help="The kit's ESPHome API on this network")
    source.add_argument("--serial", metavar="PORT", help="The kit's USB log, e.g. COM5 or /dev/ttyACM0")
    source.add_argument("--replay", metavar="FILE", type=Path, help="An ESPHome log file")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--output", required=True, help="The private study directory, outside any Git checkout")
    args = parser.parse_args(argv)
    stopping = []
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stopping.append(True))
    writer = None
    try:
        root = private_output(args.output)
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        target = parse_target(args.esphome) if args.esphome else None
        writer = HourlyJsonl(root, subdir="mmwave", manifest="mmwave-manifest.jsonl")

        def emit(record):
            writer.write(record["t_ns"], record)
            writer.flush()

        print(json.dumps({"status": "RECORDING", "output": str(root)}))
        if target:
            run_esphome(target, emit, stop=lambda: bool(stopping))
        elif args.serial:
            run_lines(serial_lines(args.serial, args.baud), emit, stop=lambda: bool(stopping), label="serial")
        else:
            with open(args.replay, encoding="utf-8", errors="replace") as log:
                run_lines(log, emit, stop=lambda: bool(stopping), label="replay")
    except SourceError as error:
        remedies = {
            "esphome_encrypted": "The kit's API is encrypted. Use --serial with its USB cable, or remove "
                                 "`api: encryption:` from its ESPHome config on a trusted network.",
            "esphome_password_required": "The kit's API has a password. Use --serial, or remove `api: password:`.",
        }
        print(json.dumps({"status": "ERROR", "reason": str(error), "remedy": remedies.get(str(error))}))
        return 1
    except (OutputError, OSError, ValueError) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1
    finally:
        if writer:
            writer.finish()
    print(json.dumps({"status": "STOPPED"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

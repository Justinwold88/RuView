#!/usr/bin/env python3
"""Record the sensing server's loopback UDP tee to hourly nanosecond pcap files.

Real-home dataset protocol (docs/validation/real-home-dataset-protocol.md).
Python standard library only. Listens on a loopback address only and writes
each datagram unchanged (pcap link type 147, USER0) with its receive time.

One file is written per UTC hour, as <output>/<YYYY-MM-DD>/<YYYY-MM-DDTHH>Z.pcap.
A file is written as .partial and renamed when it closes. Its SHA-256,
datagram counts per magic and node, and first/last timestamps are appended to
<output>/manifest.jsonl.

The output directory must be outside any Git work tree: captures are person
data and must never be committed. No addresses are recorded.
"""
from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import shutil
import signal
import socket
import struct
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

LINKTYPE_USER0 = 147
SNAPLEN = 65535
PCAP_HEADER = struct.pack("<IHHiIII", 0xA1B23C4D, 2, 4, 0, 0, SNAPLEN, LINKTYPE_USER0)
CSI_MAGIC = 0xC5110001
MAX_DATAGRAM = 65535
STATS_SECONDS = 600
DISK_CHECK_SECONDS = 60
CLOCK_JUMP_NS = 2_000_000_000


class RecorderError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise RecorderError(reason)


def private_output(value):
    """Resolve the output directory; refuse links and Git work trees."""
    path = Path(value).expanduser().absolute()
    require(not any(p.is_symlink() for p in (path, *path.parents)), "output_contains_link")
    path = path.resolve()
    require(not any((p / ".git").exists() for p in (path, *path.parents)), "output_must_be_outside_git")
    return path


def loopback_address(value):
    host, _, port = value.rpartition(":")
    host = host.strip("[]")
    require(host and port.isdigit() and 0 < int(port) < 65536, "listen_must_be_host_port")
    require(ipaddress.ip_address(host).is_loopback, "listen_must_be_loopback")
    return host, int(port)


def classify(datagram):
    """(magic hex, node id or None). Node id is read only for ADR-018 CSI frames."""
    if len(datagram) < 4:
        return "short", None
    magic = struct.unpack_from("<I", datagram)[0]
    node = datagram[4] if magic == CSI_MAGIC and len(datagram) >= 5 else None
    return f"0x{magic:08X}", node


def hour_key(ns):
    stamp = datetime.fromtimestamp(ns // 1_000_000_000, tz=timezone.utc)
    return stamp.strftime("%Y-%m-%d"), stamp.strftime("%Y-%m-%dT%H") + "Z"


class HourlyPcap:
    """Writes one pcap per UTC hour; closes with a manifest line."""

    def __init__(self, root, manifest):
        self.root, self.manifest = root, manifest
        self.key = self.stream = self.digest = None
        self.partial = self.final = None

    def _open(self, ns):
        day, name = hour_key(ns)
        directory = self.root / day
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        # A restart within the same hour, or a .partial left by a crash, gets a
        # new suffix; nothing already on disk is overwritten.
        for restart in range(100):
            stem = name if restart == 0 else f"{name}-r{restart}"
            self.final = directory / f"{stem}.pcap"
            self.partial = directory / f"{stem}.pcap.partial"
            if not self.final.exists() and not self.partial.exists():
                break
        else:
            raise RecorderError("too_many_restarts_in_one_hour")
        self.stream = open(self.partial, "xb")
        self.digest = hashlib.sha256()
        self.key = (day, name)
        self.counts, self.nodes = {}, {}
        self.datagrams = self.bytes = 0
        self.first = self.last = None
        self._write(PCAP_HEADER)

    def _write(self, data):
        self.stream.write(data)
        self.digest.update(data)
        self.bytes += len(data)

    def write(self, ns, datagram):
        if self.key != hour_key(ns):
            self.close()
            self._open(ns)
        seconds, fraction = divmod(ns, 1_000_000_000)
        self._write(struct.pack("<IIII", seconds, fraction, len(datagram), len(datagram)) + datagram)
        magic, node = classify(datagram)
        self.counts[magic] = self.counts.get(magic, 0) + 1
        if node is not None:
            self.nodes[str(node)] = self.nodes.get(str(node), 0) + 1
        self.datagrams += 1
        self.first = ns if self.first is None else self.first
        self.last = ns

    def flush(self):
        if self.stream:
            self.stream.flush()

    def close(self, note=None):
        if not self.stream:
            return None
        self.stream.flush()
        os.fsync(self.stream.fileno())
        self.stream.close()
        os.replace(self.partial, self.final)
        entry = {"file": self.final.relative_to(self.root).as_posix(), "bytes": self.bytes,
                 "sha256": self.digest.hexdigest(), "datagrams": self.datagrams,
                 "by_magic": dict(sorted(self.counts.items())), "csi_by_node": dict(sorted(self.nodes.items())),
                 "first_ns": self.first, "last_ns": self.last, "closed_utc": datetime.now(timezone.utc).isoformat()}
        if note:
            entry["note"] = note
        self.manifest.write(json.dumps(entry, sort_keys=True) + "\n")
        self.manifest.flush()
        os.fsync(self.manifest.fileno())
        self.stream = self.key = None
        return entry


def record(sock, root, *, clock=time.time_ns, monotonic=time.monotonic_ns, min_free_bytes=5 * 1024**3,
           stop=lambda: False, log=print, max_datagrams=None):
    """Receive until stop() or the disk limit; returns a summary dict."""
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    with open(root / "manifest.jsonl", "a") as manifest:
        writer = HourlyPcap(root, manifest)
        events = open(root / "events.jsonl", "a")
        total, window = 0, {}
        last_stats = last_disk = last_flush = monotonic()
        offset = None
        reason = "stopped"
        try:
            while not stop():
                try:
                    datagram = sock.recv(MAX_DATAGRAM + 1)
                except (socket.timeout, BlockingIOError):
                    datagram = None
                now_mono = monotonic()
                if datagram is not None:
                    if len(datagram) > MAX_DATAGRAM:
                        continue
                    now = clock()
                    jump = 0 if offset is None else (now - now_mono) - offset
                    if offset is None or abs(jump) > CLOCK_JUMP_NS:
                        if offset is not None:
                            events.write(json.dumps({"event": "clock_jump", "ns": now, "jump_ns": jump}) + "\n")
                            events.flush()
                        offset = now - now_mono
                    writer.write(now, datagram)
                    magic, node = classify(datagram)
                    key = magic if node is None else f"{magic}/node{node}"
                    window[key] = window.get(key, 0) + 1
                    total += 1
                    if max_datagrams is not None and total >= max_datagrams:
                        break
                if now_mono - last_flush >= 1_000_000_000:
                    writer.flush()
                    last_flush = now_mono
                if now_mono - last_disk >= DISK_CHECK_SECONDS * 1_000_000_000:
                    last_disk = now_mono
                    if shutil.disk_usage(root).free < min_free_bytes:
                        reason = "disk_free_below_limit"
                        log(json.dumps({"stop": reason}))
                        break
                if now_mono - last_stats >= STATS_SECONDS * 1_000_000_000:
                    seconds = (now_mono - last_stats) / 1e9
                    log(json.dumps({"rates_per_s": {k: round(v / seconds, 2) for k, v in sorted(window.items())},
                                    "total": total}))
                    window, last_stats = {}, now_mono
        finally:
            writer.close(note=reason)
            events.close()
    return {"datagrams": total, "reason": reason}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--listen", default="127.0.0.1:5600", help="Loopback ip:port given to --udp-tee")
    parser.add_argument("--output", required=True, help="Private directory outside any Git checkout")
    parser.add_argument("--min-free-gb", type=float, default=5.0, help="Stop when free disk space falls below this")
    args = parser.parse_args(argv)
    try:
        require(0 <= args.min_free_gb <= 10_000, "invalid_min_free_gb")
        host, port = loopback_address(args.listen)
        root = private_output(args.output)
        family = socket.AF_INET6 if ":" in host else socket.AF_INET
        sock = socket.socket(family, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
        sock.bind((host, port))
        sock.settimeout(0.5)
    except (RecorderError, OSError, ValueError) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1
    stopping = []
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stopping.append(True))
    print(json.dumps({"status": "RECORDING", "listen": f"{host}:{port}", "output": str(root)}))
    summary = record(sock, root, min_free_bytes=int(args.min_free_gb * 1024**3), stop=lambda: bool(stopping))
    print(json.dumps({"status": "STOPPED", **summary}))
    return 0 if summary["reason"] == "stopped" else 2


if __name__ == "__main__":
    sys.exit(main())

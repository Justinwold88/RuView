#!/usr/bin/env python3
"""Log the sensing server's live room verdict for real-home scoring.

Real-home dataset protocol (docs/validation/real-home-dataset-protocol.md).
Python standard library only. Polls GET /api/v1/sensing/latest (default every
0.25 s) and GET /api/v1/links (every 60 s) on a loopback server. Writes compact
JSON lines, one file per UTC hour, as <output>/outputs/<YYYY-MM-DD>/<YYYY-MM-DDTHH>Z.jsonl.
Each closed file's SHA-256 goes in <output>/outputs-manifest.jsonl.

Each line holds what scoring needs and nothing else:
- the room verdict
- each node's level
- the followed link
- the breathing estimate

No amplitudes, positions or addresses are kept. A server that is unreachable
or stale is logged once per outage, so gaps in the output are visible.
"""
from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import signal
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

MAX_BODY = 4 * 1024 * 1024
LINKS_SECONDS = 60
TIMEOUT = 2.0


class OutputError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise OutputError(reason)


def private_output(value):
    path = Path(value).expanduser().absolute()
    require(not any(p.is_symlink() for p in (path, *path.parents)), "output_contains_link")
    path = path.resolve()
    require(not any((p / ".git").exists() for p in (path, *path.parents)), "output_must_be_outside_git")
    return path


def loopback_url(value):
    parsed = urllib.parse.urlsplit(value)
    require(parsed.scheme in ("http", "https") and parsed.hostname, "url_must_be_http")
    host = parsed.hostname
    require(host == "localhost" or ipaddress.ip_address(host).is_loopback, "url_must_be_loopback")
    return value.rstrip("/")


def compact(update, polled_ns):
    """Reduce one sensing update to the scoring fields; None for a stale or empty server."""
    if not isinstance(update, dict) or "classification" not in update:
        return {"polled_ns": polled_ns, "status": str(update.get("status", "unknown"))[:40]
                if isinstance(update, dict) else "invalid"}
    cls = update.get("classification") or {}
    nodes = {}
    for node in update.get("nodes") or []:
        inference = node.get("node_inference") or {}
        if "node_id" in node:
            nodes[str(node["node_id"])] = {"level": inference.get("classification"),
                                           "age_ms": inference.get("age_ms")}
    vitals = update.get("vital_signs") or {}
    return {"polled_ns": polled_ns, "t": update.get("timestamp"), "tick": update.get("tick"),
            "source": update.get("source"), "presence": bool(cls.get("presence")),
            "level": cls.get("motion_level"), "confidence": cls.get("confidence"), "nodes": nodes,
            "breathing_bpm": vitals.get("breathing_rate_bpm"),
            "breathing_confidence": vitals.get("breathing_confidence")}


def hour_key(ns):
    stamp = datetime.fromtimestamp(ns // 1_000_000_000, tz=timezone.utc)
    return stamp.strftime("%Y-%m-%d"), stamp.strftime("%Y-%m-%dT%H") + "Z"


class HourlyJsonl:
    def __init__(self, root):
        self.root = root
        self.manifest = open(root / "outputs-manifest.jsonl", "a")
        self.key = self.stream = None

    def _open(self, ns):
        day, name = hour_key(ns)
        directory = self.root / "outputs" / day
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        for restart in range(100):
            stem = name if restart == 0 else f"{name}-r{restart}"
            self.path = directory / f"{stem}.jsonl"
            if not self.path.exists():
                break
        else:
            raise OutputError("too_many_restarts_in_one_hour")
        self.stream = open(self.path, "x")
        self.digest, self.lines, self.key = hashlib.sha256(), 0, (day, name)

    def write(self, ns, record):
        if self.key != hour_key(ns):
            self.close()
            self._open(ns)
        line = json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
        self.stream.write(line)
        self.digest.update(line.encode())
        self.lines += 1

    def flush(self):
        if self.stream:
            self.stream.flush()

    def close(self):
        if self.stream:
            self.stream.flush()
            os.fsync(self.stream.fileno())
            self.stream.close()
            self.manifest.write(json.dumps({"file": self.path.relative_to(self.root).as_posix(),
                                            "lines": self.lines, "sha256": self.digest.hexdigest()}) + "\n")
            self.manifest.flush()
            self.stream = self.key = None

    def finish(self):
        self.close()
        self.manifest.close()


def fetch(url, token, timeout=TIMEOUT):
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310 (loopback only)
        body = response.read(MAX_BODY + 1)
    require(len(body) <= MAX_BODY, "response_too_large")
    return json.loads(body)


def run(base, root, *, token=None, interval=0.25, get=fetch, clock=time.time_ns, sleep=time.sleep,
        stop=lambda: False, max_polls=None):
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    writer = HourlyJsonl(root)
    last_tick, outage, polls, next_links = None, None, 0, 0
    try:
        while not stop() and (max_polls is None or polls < max_polls):
            started = clock()
            polls += 1
            try:
                record = compact(get(base + "/api/v1/sensing/latest", token), started)
                if "status" in record:
                    if outage != record["status"]:
                        writer.write(started, record)
                        outage = record["status"]
                elif record["tick"] != last_tick:
                    writer.write(started, record)
                    last_tick, outage = record["tick"], None
                if started >= next_links:
                    links = get(base + "/api/v1/links", token)
                    writer.write(started, {"polled_ns": started, "links": {
                        "mode": links.get("mode"), "selected": links.get("selected")}})
                    next_links = started + LINKS_SECONDS * 1_000_000_000
            except (urllib.error.URLError, OSError, ValueError) as error:
                reason = "unreachable" if isinstance(error, (urllib.error.URLError, OSError)) else "invalid_response"
                if outage != reason:
                    writer.write(started, {"polled_ns": started, "status": reason})
                    outage = reason
            writer.flush()
            sleep(max(0.0, interval - (clock() - started) / 1e9))
    finally:
        writer.finish()
    return polls


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--url", default="http://127.0.0.1:8080", help="Sensing server on this host")
    parser.add_argument("--output", required=True, help="Private directory outside any Git checkout")
    parser.add_argument("--interval", type=float, default=0.25, help="Seconds between polls (0.1 to 5)")
    parser.add_argument("--token-file", help="File holding the API bearer token, if the server requires one")
    args = parser.parse_args(argv)
    try:
        require(0.1 <= args.interval <= 5.0, "interval_out_of_range")
        base = loopback_url(args.url)
        root = private_output(args.output)
        token = Path(args.token_file).read_text().strip() if args.token_file else None
    except (OutputError, OSError, ValueError) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1
    stopping = []
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stopping.append(True))
    print(json.dumps({"status": "LOGGING", "url": base, "output": str(root)}))
    polls = run(base, root, token=token, interval=args.interval, stop=lambda: bool(stopping))
    print(json.dumps({"status": "STOPPED", "polls": polls}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

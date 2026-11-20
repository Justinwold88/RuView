#!/usr/bin/env python3
"""Append and check real-home dataset labels (docs/validation/real-home-dataset-protocol.md).

Python standard library only. The label file is a CSV of timestamped events:

    time_utc,room,event,spot,source,note

  mark   appends one event, using the host clock unless --at is given.
  check  pairs start/end events and finds overlaps. It totals each protocol
         day (04:00 to 04:00 local) and the train/test split, then tests the
         minimum test data. With --intervals it writes the guarded intervals
         that plan item 2.4 scores.

Labels carry no names: use P1, P2 in notes if anything at all.
"""
from __future__ import annotations

import argparse
import csv
import io
import json
import re
import sys
from datetime import datetime, timedelta, timezone
from datetime import time as dtime
from pathlib import Path
from zoneinfo import ZoneInfo

FIELDS = ("time_utc", "room", "event", "spot", "source", "note")
STATES = ("home_empty", "room_empty", "still", "sleep", "pet_only", "exclude")
EVENTS = frozenset([f"{s}_{edge}" for s in STATES for edge in ("start", "end")] + ["walk_in"])
WALK_IN_SOURCES = ("door", "button", "mmwave")
# Guard bands in seconds (start, end), frozen in the protocol.
GUARDS = {"home_empty": (120, 30), "room_empty": (120, 30), "still": (30, 30), "sleep": (30, 30),
          "pet_only": (0, 0), "exclude": (0, 0)}
# States that cannot overlap each other in one room.
EXCLUSIVE = (("home_empty", "room_empty", "still", "sleep", "pet_only"),)
DAY_BOUNDARY = dtime(4, 0)
TRAIN_DAYS = 3
MAX_EXCLUDED_HOURS = 6.0
MINIMUM = {"empty_home_hours": 24.0, "still_minutes_in_area": 120.0, "walk_ins": 30}
WALK_IN_MIN_EMPTY = 120.0
WALK_IN_LEAD_TOLERANCE = 2.0
MAX_NOTE = 200
MAX_ROWS = 200_000
TOKEN = re.compile(r"[A-Za-z0-9_.-]{1,40}\Z")


class LabelError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise LabelError(reason)


def parse_time(value):
    require(isinstance(value, str) and value.endswith("Z"), "time_must_be_utc_iso_with_z")
    try:
        stamp = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise LabelError("invalid_time") from error
    return stamp


def format_time(stamp):
    return stamp.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def validate_row(row, number):
    where = f"row {number}"
    require(set(row) == set(FIELDS), f"{where}: unexpected_columns")
    stamp = parse_time(row["time_utc"])
    require(TOKEN.fullmatch(row["room"] or "") is not None, f"{where}: invalid_room")
    require(row["event"] in EVENTS, f"{where}: unknown_event {row['event']!r}")
    if row["event"].startswith("still_"):
        require(re.fullmatch(r"S[0-9]{1,3}(-out)?", row["spot"] or "") is not None,
                f"{where}: still_needs_spot_like_S1_or_S1-out")
    else:
        require(not row["spot"], f"{where}: spot_only_for_still")
    if row["event"] == "walk_in":
        require(row["source"] in WALK_IN_SOURCES, f"{where}: walk_in_needs_source")
    else:
        require(not row["source"], f"{where}: source_only_for_walk_in")
    require(len(row["note"] or "") <= MAX_NOTE and "\n" not in (row["note"] or ""), f"{where}: note_too_long")
    return stamp


def read_labels(path):
    text = Path(path).read_text(encoding="utf-8")
    reader = csv.DictReader(io.StringIO(text))
    require(tuple(reader.fieldnames or ()) == FIELDS, "header_must_be " + ",".join(FIELDS))
    rows = []
    for number, row in enumerate(reader, 2):
        require(len(rows) < MAX_ROWS, "too_many_rows")
        rows.append((validate_row(row, number), row))
    require(all(a[0] <= b[0] for a, b in zip(rows, rows[1:])), "rows_must_be_in_time_order")
    return rows


def intervals_from(rows):
    """Pair start/end events per (room, state, spot). Returns (intervals, walk_ins)."""
    open_states, intervals, walk_ins = {}, [], []
    for stamp, row in rows:
        event, room = row["event"], row["room"]
        if event == "walk_in":
            walk_ins.append({"room": room, "time": stamp, "source": row["source"]})
            continue
        state, edge = event.rsplit("_", 1)
        key = (room, state)
        if edge == "start":
            require(key not in open_states, f"{event} at {row['time_utc']}: already_open")
            open_states[key] = (stamp, row["spot"])
        else:
            require(key in open_states, f"{event} at {row['time_utc']}: end_without_start")
            start, spot = open_states.pop(key)
            require(not spot or spot == row["spot"], f"{event} at {row['time_utc']}: spot_mismatch")
            intervals.append({"room": room, "state": state, "start": start, "end": stamp, "spot": spot or None})
    require(not open_states, "unclosed: " + ", ".join(f"{r}/{s}" for r, s in sorted(open_states)))
    for group in EXCLUSIVE:
        spans = sorted((i for i in intervals if i["state"] in group), key=lambda i: (i["room"], i["start"]))
        for a, b in zip(spans, spans[1:]):
            require(a["room"] != b["room"] or b["start"] >= a["end"],
                    f"overlap: {a['state']} and {b['state']} at {format_time(b['start'])}")
    return intervals, walk_ins


def guarded(interval):
    head, tail = GUARDS[interval["state"]]
    start = interval["start"] + timedelta(seconds=head)
    end = interval["end"] - timedelta(seconds=tail)
    return (start, end) if end > start else None


def subtract(span, cuts):
    """Remove every cut span from one span; returns the remaining pieces."""
    pieces = [span]
    for cut_start, cut_end in cuts:
        nxt = []
        for start, end in pieces:
            if cut_end <= start or cut_start >= end:
                nxt.append((start, end))
                continue
            if start < cut_start:
                nxt.append((start, cut_start))
            if cut_end < end:
                nxt.append((cut_end, end))
        pieces = nxt
    return pieces


def day_of(stamp, zone, first_day):
    local = stamp.astimezone(zone)
    day = (local - timedelta(hours=DAY_BOUNDARY.hour, minutes=DAY_BOUNDARY.minute)).date()
    return (day - first_day).days + 1


def day_bounds(day_number, zone, first_day):
    date = first_day + timedelta(days=day_number - 1)
    start = datetime.combine(date, DAY_BOUNDARY, tzinfo=zone)
    end = datetime.combine(date + timedelta(days=1), DAY_BOUNDARY, tzinfo=zone)
    return start.astimezone(timezone.utc), end.astimezone(timezone.utc)


def summarize(rows, zone, in_area_spots=None):
    """Per-day totals, split and minimum-data check; plus guarded scoring intervals."""
    intervals, walk_ins = intervals_from(rows)
    require(rows, "no_labels")
    first_local = rows[0][0].astimezone(zone) - timedelta(hours=DAY_BOUNDARY.hour, minutes=DAY_BOUNDARY.minute)
    first_day = first_local.date()
    excludes = [(i["start"], i["end"]) for i in intervals if i["state"] == "exclude"]
    days = {}

    def bucket(number):
        return days.setdefault(number, {"empty_home_hours": 0.0, "room_empty_hours": 0.0,
                                        "still_minutes_in_area": 0.0, "still_minutes_outside": 0.0,
                                        "sleep_hours": 0.0, "pet_only_minutes": 0.0, "walk_ins": 0,
                                        "walk_ins_without_empty_lead": 0, "excluded_hours": 0.0})

    scoring = []
    for interval in intervals:
        if interval["state"] == "exclude":
            pieces = [(interval["start"], interval["end"])]
        else:
            span = guarded(interval)
            pieces = subtract(span, excludes) if span else []
        for start, end in pieces:
            # Split each piece at day boundaries so every day gets its own share.
            cursor = start
            while cursor < end:
                number = day_of(cursor, zone, first_day)
                _, day_end = day_bounds(number, zone, first_day)
                stop = min(end, day_end)
                seconds = (stop - cursor).total_seconds()
                totals = bucket(number)
                state = interval["state"]
                if state == "home_empty":
                    totals["empty_home_hours"] += seconds / 3600
                elif state == "room_empty":
                    totals["room_empty_hours"] += seconds / 3600
                elif state == "still":
                    inside = in_area_spots is None or interval["spot"] in in_area_spots
                    inside = inside and not interval["spot"].endswith("-out")
                    totals["still_minutes_in_area" if inside else "still_minutes_outside"] += seconds / 60
                elif state == "sleep":
                    totals["sleep_hours"] += seconds / 3600
                elif state == "pet_only":
                    totals["pet_only_minutes"] += seconds / 60
                else:
                    totals["excluded_hours"] += seconds / 3600
                if state != "exclude":
                    item = {"room": interval["room"], "state": state, "spot": interval["spot"],
                            "day": number, "start": format_time(cursor), "end": format_time(stop)}
                    if state == "still":
                        item["in_area"] = inside
                    scoring.append(item)
                cursor = stop
    scored_walk_ins = []
    occupied = [i for i in intervals if i["state"] in ("home_empty", "still", "sleep", "pet_only")]
    for walk in walk_ins:
        number = day_of(walk["time"], zone, first_day)
        totals = bucket(number)
        require(not any(i["room"] == walk["room"] and i["start"] < walk["time"] < i["end"] for i in occupied),
                f"walk_in at {format_time(walk['time'])}: inside_a_state_that_is_not_room_empty")
        if any(s <= walk["time"] < e for s, e in excludes):
            continue
        lead = [i for i in intervals if i["state"] == "room_empty" and i["room"] == walk["room"]
                and abs((i["end"] - walk["time"]).total_seconds()) <= WALK_IN_LEAD_TOLERANCE]
        lead_seconds = max(((i["end"] - i["start"]).total_seconds() for i in lead), default=0.0)
        if lead_seconds < WALK_IN_MIN_EMPTY:
            totals["walk_ins_without_empty_lead"] += 1
            continue
        totals["walk_ins"] += 1
        scored_walk_ins.append({"room": walk["room"], "time": format_time(walk["time"]),
                                "source": walk["source"], "day": number, "prior_room_empty_seconds": lead_seconds})
    split = {}
    for number in sorted(days):
        totals = days[number]
        dropped = totals["excluded_hours"] > MAX_EXCLUDED_HOURS
        split[number] = "dropped" if dropped else ("train" if number <= TRAIN_DAYS else "test")
        totals["split"] = split[number]
        for key, value in totals.items():
            if isinstance(value, float):
                totals[key] = round(value, 3)
    test = [days[n] for n in days if split[n] == "test"]
    test_totals = {key: round(sum(d[key] for d in test), 3) for key in MINIMUM}
    missing = {key: {"have": test_totals[key], "need": need} for key, need in MINIMUM.items()
               if test_totals[key] < need}
    if sum(1 for n in split if split[n] == "test") == 0:
        missing["test_days"] = {"have": 0, "need": 1}
    report = {"first_day_local": first_day.isoformat(), "timezone": str(zone),
              "day_boundary_local": DAY_BOUNDARY.strftime("%H:%M"),
              "days": {str(n): days[n] for n in sorted(days)},
              "test_totals": test_totals, "minimum_test_data_met": not missing, "missing": missing}
    for item in scoring:
        item["split"] = split[item["day"]]
    for item in scored_walk_ins:
        item["split"] = split[item["day"]]
    return report, {"intervals": scoring, "walk_ins": scored_walk_ins}


def open_states(rows):
    current = {}
    for _, row in rows:
        if row["event"] != "walk_in":
            state, edge = row["event"].rsplit("_", 1)
            if edge == "start":
                current[(row["room"], state)] = row
            else:
                current.pop((row["room"], state), None)
    return current


def append_row(path, row):
    """Append one event. A walk_in also closes the room's open room_empty at the same time."""
    path = Path(path)
    new = not path.exists() or path.stat().st_size == 0
    rows = [] if new else read_labels(path)
    require(not rows or rows[-1][0] <= parse_time(row["time_utc"]), "new_label_is_older_than_last_row")
    validate_row(row, "new")
    out = [row]
    if row["event"] == "walk_in" and (row["room"], "room_empty") in open_states(rows):
        out.insert(0, dict(row, event="room_empty_end", source="", note=""))
    with path.open("a", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=FIELDS, lineterminator="\n")
        if new:
            writer.writeheader()
        writer.writerows(out)
    return out


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    mark = sub.add_parser("mark", help="Append one labelled event")
    mark.add_argument("event", choices=sorted(EVENTS))
    mark.add_argument("--labels", required=True)
    mark.add_argument("--room", default="study")
    mark.add_argument("--spot", default="", help="Still spot, e.g. S1 (S1-out if outside the sensing area)")
    mark.add_argument("--source", default="", choices=("",) + WALK_IN_SOURCES)
    mark.add_argument("--note", default="")
    mark.add_argument("--at", help="UTC time like 2026-12-01T18:30:05.250Z; default now")
    check = sub.add_parser("check", help="Validate the file and total each day")
    check.add_argument("--labels", required=True)
    check.add_argument("--timezone", required=True, help="IANA zone of the home, e.g. America/Los_Angeles")
    check.add_argument("--in-area", default=None, help="Comma-separated still spots inside the sensing area")
    check.add_argument("--intervals", type=Path, help="Write guarded scoring intervals (private) to this JSON")
    args = parser.parse_args(argv)
    try:
        if args.command == "mark":
            at = args.at or format_time(datetime.now(timezone.utc))
            written = append_row(args.labels, {"time_utc": at, "room": args.room, "event": args.event,
                                               "spot": args.spot, "source": args.source, "note": args.note})
            print(json.dumps({"status": "MARKED", "events": [r["event"] for r in written], "time_utc": at}))
            return 0
        spots = None if args.in_area is None else {s.strip() for s in args.in_area.split(",") if s.strip()}
        report, scoring = summarize(read_labels(args.labels), ZoneInfo(args.timezone), spots)
        if args.intervals:
            args.intervals.write_text(json.dumps(scoring, indent=1) + "\n")
        print(json.dumps(report, indent=2))
        return 0 if report["minimum_test_data_met"] else 3
    except (LabelError, OSError, ValueError) as error:
        print(json.dumps({"status": "ERROR", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    sys.exit(main())

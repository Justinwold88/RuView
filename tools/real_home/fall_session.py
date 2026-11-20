#!/usr/bin/env python3
"""Run one staged fall-study session (docs/validation/fall-study-protocol.md).

Python standard library only. Shuffles the frozen trial list with a recorded
seed and prompts the spotter for each trial. The spotter presses Enter at the
moment of impact or contact, or types `a` and Enter to abort the trial.
Normal-activity blocks record their start and end.

Each trial is appended to a CSV at once, so an interrupted session keeps what
it has. The participant is recorded as P1, P2 and so on, never by name.
"""
from __future__ import annotations

import argparse
import csv
import json
import random
import sys
from datetime import datetime, timezone
from pathlib import Path

FALLS = ("fall_forward", "fall_backward", "fall_sideways", "fall_from_chair")
FALL_LIKE = ("sit_fast", "lie_down_fast", "drop_object", "jump", "pick_up_fast")
ACTIVITY = ("walk_around", "chores")
REPEATS = 3
ACTIVITY_SECONDS = 600
FIELDS = ("session", "participant", "seed", "trial", "kind", "group", "cue_utc", "mark_utc", "end_utc", "status")


class SessionError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise SessionError(reason)


def now_utc():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def plan(seed, falls=True):
    """Frozen trial list: 3 of each kind, shuffled; activity blocks at the end."""
    trials = [(kind, "fall") for kind in FALLS for _ in range(REPEATS)] if falls else []
    trials += [(kind, "fall_like") for kind in FALL_LIKE for _ in range(REPEATS)]
    random.Random(seed).shuffle(trials)
    return trials + [(kind, "activity") for kind in ACTIVITY]


def outside_git(path):
    path = Path(path).expanduser().absolute()
    require(not any((p / ".git").exists() for p in path.parents), "session_file_must_be_outside_git")
    return path


def run(path, session, participant, seed, falls=True, ask=input, clock=now_utc, wait=None, say=print):
    """Prompt through the plan; append one CSV row per trial or block."""
    import time
    wait = wait or time.sleep
    path = outside_git(path)
    new = not path.exists() or path.stat().st_size == 0
    trials = plan(seed, falls)
    with path.open("a", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=FIELDS, lineterminator="\n")
        if new:
            writer.writeheader()
        for number, (kind, group) in enumerate(trials, 1):
            row = {"session": session, "participant": participant, "seed": seed, "trial": number,
                   "kind": kind, "group": group, "cue_utc": "", "mark_utc": "", "end_utc": "", "status": ""}
            if group == "activity":
                ask(f"[{number}/{len(trials)}] {kind}: {ACTIVITY_SECONDS // 60} minutes of normal activity. "
                    "Press Enter to start.")
                row["cue_utc"] = clock()
                wait(ACTIVITY_SECONDS)
                row["end_utc"], row["status"] = clock(), "completed"
            else:
                ask(f"[{number}/{len(trials)}] Next: {kind}. Press Enter to cue it.")
                row["cue_utc"] = clock()
                say("  Go. Press Enter at impact or contact, or type a and Enter to abort.")
                answer = ask("").strip().lower()
                row["mark_utc"] = clock()
                row["status"] = "aborted" if answer == "a" else "completed"
                say("  Stay still for 10 s, then reset. Wait at least 20 s before the next trial.")
                wait(20)
            writer.writerow(row)
            stream.flush()
    return len(trials)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--file", required=True, help="Session CSV in the private study directory")
    parser.add_argument("--session", required=True, help="Session id, e.g. F1")
    parser.add_argument("--participant", required=True, help="P1, P2 ... never a name")
    parser.add_argument("--seed", type=int, help="Shuffle seed; default from the clock, and recorded")
    parser.add_argument("--no-falls", action="store_true", help="Fall-like and normal activity only")
    args = parser.parse_args(argv)
    try:
        require(args.participant[:1] == "P" and args.participant[1:].isdigit(), "participant_must_be_like_P1")
        require(args.session.replace("-", "").isalnum() and len(args.session) <= 20, "invalid_session_id")
        seed = args.seed if args.seed is not None else int(datetime.now(timezone.utc).timestamp())
        print(json.dumps({"status": "SESSION", "seed": seed}))
        trials = run(args.file, args.session, args.participant, seed, falls=not args.no_falls)
    except (SessionError, OSError, EOFError, KeyboardInterrupt) as error:
        print(json.dumps({"status": "STOPPED", "reason": str(error) or type(error).__name__}))
        return 1
    print(json.dumps({"status": "DONE", "trials": trials}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

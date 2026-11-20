# Real-home dataset tools

Tools for the [real-home dataset protocol](../../docs/validation/real-home-dataset-protocol.md)
(plan item 2.3). Python standard library only.

- `record_tee.py` records the sensing server's loopback UDP tee (`--udp-tee`) to
  one nanosecond pcap per UTC hour. Datagrams are kept unchanged and no
  addresses are stored. Each closed file's SHA-256 and per-type counts go in
  `manifest.jsonl`. A `.pcap.partial` file means the recorder stopped without
  closing it; its records are valid up to the last flush (at most 1 s lost).
- `log_outputs.py` logs the server's live room verdict for scoring. It polls
  `/api/v1/sensing/latest` every 0.25 s and `/api/v1/links` every minute,
  keeping only the room verdict, node levels, the followed link and the
  breathing estimate. It writes hourly JSON lines with a SHA-256 manifest and
  logs outages once each.
- `labels.py mark` appends a timestamped label event. `labels.py check`
  validates the label file, totals each protocol day, applies the train/test
  split and tests the minimum test data. With `--intervals` it writes the
  guarded intervals that plan item 2.4 scores.

```bash
# Server: add --udp-tee 127.0.0.1:5600 to the usual command line
python tools/real_home/record_tee.py --listen 127.0.0.1:5600 --output /private/ruview-home
python tools/real_home/log_outputs.py --url http://127.0.0.1:8080 --output /private/ruview-home

python tools/real_home/labels.py mark home_empty_start --labels /private/ruview-home/labels.csv
python tools/real_home/labels.py mark walk_in --source door --labels /private/ruview-home/labels.csv
python tools/real_home/labels.py check --labels /private/ruview-home/labels.csv \
  --timezone America/Los_Angeles --in-area S1,S2
```

## Scoring (plan item 2.4)

`score_g2.py` applies the protocol's frozen scoring rules to the output log,
the guarded intervals and, if present, the tee capture's mmWave breathing
values. It checks every file against its manifest's SHA-256 first. Test days
need `--freeze-commit` and are scored once; a second test scoring needs
`--rescore-reason`, which is recorded in `scoring/test-scored.json`.

```bash
python tools/real_home/labels.py check --labels /private/ruview-home/labels.csv \
  --timezone America/Los_Angeles --in-area S1,S2 --intervals /private/ruview-home/intervals.json
python tools/real_home/score_g2.py --data /private/ruview-home --intervals /private/ruview-home/intervals.json \
  --split test --freeze-commit <sha> --report g2.md --json g2.json
```

Captures and labels are person data. Keep them in a private directory outside
any Git checkout; `record_tee.py` refuses an output directory inside one.

Tests are SYNTHETIC: `python -B -m unittest discover -s tools/real_home -v`.

# Real-home dataset tools

Tools for the [real-home dataset protocol](../../docs/validation/real-home-dataset-protocol.md)
(plan item 2.3). Python standard library only.

- `record_tee.py` records the sensing server's loopback UDP tee (`--udp-tee`) to
  one nanosecond pcap per UTC hour. Datagrams are kept unchanged and no
  addresses are stored. Each closed file's SHA-256 and per-type counts go in
  `manifest.jsonl`. A `.pcap.partial` file means the recorder stopped without
  closing it; its records are valid up to the last flush (at most 1 s lost).
- `labels.py mark` appends a timestamped label event. `labels.py check`
  validates the label file, totals each protocol day, applies the train/test
  split and tests the minimum test data. With `--intervals` it writes the
  guarded intervals that plan item 2.4 scores.

```bash
# Server: add --udp-tee 127.0.0.1:5600 to the usual command line
python tools/real_home/record_tee.py --listen 127.0.0.1:5600 --output /private/ruview-home

python tools/real_home/labels.py mark home_empty_start --labels /private/ruview-home/labels.csv
python tools/real_home/labels.py mark walk_in --source door --labels /private/ruview-home/labels.csv
python tools/real_home/labels.py check --labels /private/ruview-home/labels.csv \
  --timezone America/Los_Angeles --in-area S1,S2
```

Captures and labels are person data. Keep them in a private directory outside
any Git checkout; `record_tee.py` refuses an output directory inside one.

Tests are SYNTHETIC: `python -B -m unittest discover -s tools/real_home -v`.

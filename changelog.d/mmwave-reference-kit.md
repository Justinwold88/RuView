- **Real-home study: record the radar kit as sold (issue #51).** New
  `tools/real_home/record_mmwave.py` logs the Seeed MR60BHA2 kit's ESPHome
  readings next to the Wi-Fi capture, on the host clock. It reads the kit's
  ESPHome API read-only (private addresses only) or its USB log, and keeps
  only presence and breathing rate. `score_g2.py` uses that log as a
  breathing reference and now reports the protocol's two reference checks:
  empty-home intervals where the kit saw a person, and walk-in onsets within
  2 s. The protocol text is updated and stays PROPOSED. SYNTHETIC tests,
  including a fake ESPHome device that checks the recorder never sends a
  command.

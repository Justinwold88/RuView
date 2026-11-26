- **Fix: MQTT to a broker on another host never started.** The server
  treated the plaintext-MQTT advisory (ADR-115 §3.9: warn now, hard-fail at
  v0.8.0) as a fatal error, so `--mqtt --mqtt-host 192.168.1.10`, the Home
  Assistant guide's own command, logged `publisher not started` and published
  nothing. Only a broker on the same host worked. The advisory now logs a
  warning and the publisher starts; fatal problems (empty host, port 0, bad
  rates) still stop it. Found by the new Home Assistant app's smoke test.
  The guide's entity counts are also corrected for the link-score sensor
  (12 per device by default).

- **Fix: MQTT stopped updating Home Assistant after one lag.** The task that
  turns the sensing broadcast into MQTT snapshots ended on the first
  `Lagged` error from the broadcast, so if it ever fell more than 256
  messages behind, Home Assistant stopped getting updates with nothing in
  the log. It now skips the missed messages, logs a warning and keeps going
  (`mqtt::bridge::forward`). The new test fails on the old behaviour.

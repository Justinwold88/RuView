- **Dashboard: calibrate the empty room with one button (issue #52).** The
  Sensing tab has an *Empty-room calibration* card. **Calibrate empty room**
  starts the run (`POST /api/v1/calibration/motion/start`), counts down while
  you leave and while it records, then lists each node's presence threshold
  and 0–100 link score with its parts (SNR, frame rate, lost frames). Node
  status rows also show each calibrated node's link score. Auth failures say
  how to fix them (add the server's API token under Settings → API access).
  The service worker cache is bumped to `ruview-v3` so browsers that loaded
  the dashboard before get the new card. Thresholds and link scores stay
  SYNTHETIC-tuned.

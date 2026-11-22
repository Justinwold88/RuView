- **Privacy mode hides the signal field's breathing ring.** The ring appears
  only while the vital-sign detector reports a breathing rate, so with
  `--privacy-mode` on, the field still showed that a rate had been measured
  after the rate itself was removed. Privacy mode now never draws the ring;
  without it, nothing changes. Test:
  `signal_field_breathing_tests::privacy_mode_never_draws_the_ring`.

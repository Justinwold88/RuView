- **CI: the BFI Wireshark oracle tests now run on pull requests.**
  `bfi-research.yml` gains a job that installs tshark and editcap 4.2.2 on
  Ubuntu 24.04 and runs `test_public_bfi_motion` with
  `RUVIEW_REQUIRE_WIRESHARK=1`. Its decoder-oracle, lossless-conversion and
  census tests used to be skipped in pull-request CI and ran only inside the
  manual public BFI workflow. With the variable set, a missing or wrong
  Wireshark version fails the run instead of skipping 8 tests.

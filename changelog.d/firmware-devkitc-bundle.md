- **Firmware CI: a display-less 8 MB bundle for DevKitC-1 boards.** Bare
  ESP32-S3-DevKitC-1 boards need display support compiled out: on them the
  display build's panel probe false-positives, and CSI collapses to 0 pps.
  Until now that meant a local source build with `sdkconfig.defaults.devkitc`.
  `firmware-ci.yml` now also builds an `8mb-devkitc` variant with that overlay,
  checks that display support is off, and packages it with the other firmware
  bundles. The variant passed a CI build only; it has not been run on a board.

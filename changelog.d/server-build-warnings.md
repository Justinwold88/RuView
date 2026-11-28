- **The sensing server builds without compiler warnings.** A Linux build
  printed nine warnings and suggested `cargo fix`. Running it removes code
  that only Windows or the tests use, so a later `cargo test` or Windows
  build fails. The warnings are now fixed at the source: imports used only on
  Windows or in tests are gated to those builds, one duplicate `match` arm and
  two unused imports in the Matter scaffold are removed, and one unused
  initial value is gone. Behaviour is unchanged.

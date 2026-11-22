- **CI: a rustfmt gate that grows one crate at a time.** Most of the Rust
  workspace predates rustfmt, and reformatting all of it would bury real
  changes and make upstream syncs conflict. `rustfmt.yml` instead runs
  `cargo fmt --check` with Rust 1.89 on the crates listed in
  `v2/rustfmt-gated-crates.txt`: the 14 workspace crates that are already
  clean. Vendored patches are left out. `scripts/check_rustfmt.sh --report`
  also lists clean crates that aren't gated yet. A crate joins by being
  formatted in its own pull request.

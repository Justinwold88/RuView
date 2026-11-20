# Dependency compatibility patch

Source: crates.io `wasmtime-internal-cache` 36.0.23.
Published archive SHA-256: `e2ee46189f2ad877221274cb0c8b82cefb21ee17261ecb6c0692506e7bdd5b97` (verified against the crates.io index checksum recorded in `v2/Cargo.lock`).

Executable source, tests, fixtures and licenses are byte-for-byte upstream.
They are also identical to the previously vendored 36.0.20. Only the
normalized Cargo.toml dependency requirement changes: `zstd` from `0.13.0` to
`0.14.3`, retaining the upstream feature selection. Registry bookkeeping, the
nested lockfile, the unconsumed original manifest and upstream CI files are
omitted. No advisory suppression is added.

This preserves the parent API while selecting the patched registry dependency.
It follows Wasmtime 36.0.23, which fixes RUSTSEC-2026-0566, -0569 and -0571.
Remove this override when a compatible parent release includes the fixed
dependency.

# Dependency compatibility patch

Source: crates.io `wasmtime-internal-cache` 36.0.23.
Published archive SHA-256: `e2ee46189f2ad877221274cb0c8b82cefb21ee17261ecb6c0692506e7bdd5b97` (verified against the crates.io index checksum in `Cargo.lock`).

Executable source, tests, fixtures and licenses are byte-for-byte upstream.
Only the normalized Cargo.toml dependency requirement changes:
`zstd` from `0.13.0` to `0.14.3`, retaining the upstream feature selection.
Registry bookkeeping, the nested lockfile, the unconsumed original manifest
and upstream CI files are omitted. No advisory suppression is added.

This preserves the parent API while selecting the patched registry dependency.
Remove this override when a compatible parent release includes the fixed dependency.

History: moved from 36.0.20 on 2026-11-20 so that Wasmtime could take the
36.0.23 patch release (RUSTSEC-2026-0566, RUSTSEC-2026-0569 and
RUSTSEC-2026-0571 are fixed from 36.0.21). The executable source, tests and
license are byte-identical between 36.0.20 and 36.0.23; only the version line
of the manifest differs from the previous copy.

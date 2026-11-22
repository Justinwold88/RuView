# Maintaining this fork

[Justinwold88/RuView](https://github.com/Justinwold88/RuView) is a fork of
[ruvnet/RuView](https://github.com/ruvnet/RuView). Changes land here first,
through pull requests with passing checks. Some are offered upstream later, one
small pull request at a time, after the earlier ones have been accepted.

This page covers the fork-only automation. Everything else follows the
repository's own rules in `CLAUDE.md`, the ADRs and `CONTRIBUTING.md`.

## Pull requests

- Squash-merge fork pull requests, with a Conventional Commits title.
- The `main` branch is protected. Merging needs a pull request, resolved
  review threads and the `CSI data policy check` and `Verify fix markers`
  checks.
- Each change adds a changelog fragment, `changelog.d/<change>.md`, instead of
  editing `CHANGELOG.md`. Fragments can't conflict with each other or with
  upstream's changelog. See [changelog.d/README.md](../../changelog.d/README.md).

## Staying in sync with upstream

`.github/workflows/upstream-sync.yml` runs on Mondays and on demand. It runs
`scripts/upstream_sync.sh`, which compares upstream `main` with the fork:

| Upstream state | What the script does |
|---|---|
| Already merged | Nothing |
| Merges cleanly | Pushes `sync/upstream-<sha>`, starts the required checks and opens a pull request. If Actions may not open pull requests here, it opens an issue with the compare link instead. |
| Conflicts | Opens or updates one issue, "Upstream sync needs a manual merge", that lists the files |

Merge a sync pull request **with a merge commit, never a squash**. A merge
commit keeps upstream's commit ids in the fork's history, so the next sync only
brings in what is new. Before merging, run the tests for the crates that the
upstream commits touch. A sync pull request opened by the workflow starts only
the required checks.

To resolve a conflict by hand:

```bash
git fetch https://github.com/ruvnet/RuView.git main
git switch -c sync/upstream-$(git rev-parse --short=8 FETCH_HEAD) origin/main
git merge --no-ff FETCH_HEAD
# resolve, test, commit, push, open a pull request, merge with a merge commit
```

When upstream changes something the fork also changed, keep whichever is
stricter or newer, and say which in the merge commit. For example, the
2026-11-21 sync kept the fork's Wasmtime 36.0.23 over upstream's 36.0.21.

To preview a sync without pushing anything:

```bash
scripts/upstream_sync.sh --dry-run
```

## Releases

Fork releases use tags named **`fork-YYYY.MM.DD`**. A second release on the
same day adds `-2`, `-3` and so on. These names never start with `v`, so they
can't collide with upstream's `v*`, `v*-esp32` or `v*-pip` tags. They also
don't start the tag-triggered workflows those tags start.

To cut a release from the current `main`:

```bash
git fetch origin main
git tag fork-2026.11.21 origin/main
git push origin fork-2026.11.21
```

`.github/workflows/fork-release.yml` then runs `scripts/fork_release.sh`, which:

- refuses a tag that is not on `main` or doesn't match the naming rule
- writes a short header: what the fork is, how evidence is labelled, and
  where the firmware came from
- adds the changelog fragments created since the previous fork tag, oldest
  first
- appends GitHub's generated list of pull requests merged since the previous
  fork tag. The first release starts at `fork-base`, the upstream commit the
  fork started from.
- attaches the firmware bundles from the newest successful Firmware CI run on
  `main`, with `SHA256SUMS.txt`. It does this only when `firmware/` and
  `firmware-ci.yml` are unchanged between that run's commit and the tag.
  Otherwise the notes say no firmware is attached.
- marks the release as the latest

Firmware bundles are CI builds. A release never claims a board was validated.
Hardware evidence is a captured boot and runtime log from real silicon,
recorded in the pull request that claims it.

To preview a release (needs `GH_TOKEN` and the tag present locally):

```bash
scripts/fork_release.sh fork-2026.11.21 --dry-run
```

The `CI snapshot vNNN` releases that `ci.yml` creates on every push to `main`
are upstream's. `ci.yml` now skips them outside `ruvnet/RuView`, so the fork's
release list holds fork releases only. Snapshots made before that change are
still listed.

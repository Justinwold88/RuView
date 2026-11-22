# Changelog fragments

Each fork pull request adds one file here instead of editing `CHANGELOG.md`.

**Why.** Every pull request used to add its entry at the top of
`CHANGELOG.md`'s `[Unreleased]` section. Two open pull requests therefore
always conflicted, and so did every upstream sync that touched the changelog.
A file per change can't conflict.

**How.**
- Name the file after the change, in lower case with hyphens:
  `changelog.d/privacy-mode-breathing-ring.md`.
- Write one entry, in the same style as `CHANGELOG.md`: a bullet that starts
  with a bold one-line summary, then what changed and why. Tag accuracy and
  performance numbers MEASURED, SYNTHETIC or CLAIMED.
- A fork release puts the fragments added since the previous fork tag into its
  notes (`scripts/fork_release.sh`).
- When a change is offered upstream, its fragment's text goes into upstream's
  `CHANGELOG.md` in that pull request.

Entries that are already in `CHANGELOG.md` stay there.

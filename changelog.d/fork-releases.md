- **Fork releases, changelog fragments and a fork maintenance guide.**
  Pushing a `fork-YYYY.MM.DD` tag on `main` now publishes a release. The notes
  have a short header, then the changelog fragments added since the previous
  fork tag, then GitHub's generated list of pull requests. The release
  attaches the firmware bundles of the newest Firmware CI run whose firmware
  sources match, labelled as CI builds, not hardware validation. Fork tags
  never start with `v`, so they don't collide with upstream's tags or start
  upstream's release workflows. Fork changes now add a fragment in
  `changelog.d/` instead of editing `CHANGELOG.md`, so open pull requests and
  upstream syncs stop conflicting on the changelog. `ci.yml` no longer makes
  "CI snapshot" releases outside `ruvnet/RuView`. `docs/fork/README.md`
  covers fork pull requests, the weekly upstream sync and releases.

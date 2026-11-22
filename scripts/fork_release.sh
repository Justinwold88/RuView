#!/usr/bin/env bash
# Publish a GitHub release for a fork-YYYY.MM.DD tag.
#
# Usage: scripts/fork_release.sh <tag> [--dry-run]
#
# Run by .github/workflows/fork-release.yml when such a tag is pushed. See
# docs/fork/README.md.
#
#   - The tag must look like fork-2026.11.21 (a same-day follow-up adds -2,
#     -3, ...) and point at a commit on main. Fork tags never start with "v",
#     so they can't collide with upstream's v*, v*-esp32 or v*-pip tags or
#     start upstream's tag-triggered release workflows.
#   - The notes are a short header, the changelog fragments (changelog.d/)
#     added since the previous fork tag, and GitHub's generated list of pull
#     requests merged since then. The first release starts at fork-base, the
#     fork point (upstream 13b47883); the script creates that tag if it is
#     missing.
#   - Firmware bundles from the newest successful Firmware CI run on main are
#     attached when firmware/ and firmware-ci.yml are unchanged between that
#     run's commit and the tag. Otherwise the notes say none are attached.
#     CI builds are not hardware validation, and the notes say so.
#
# --dry-run prints the plan and publishes nothing. It still needs GH_TOKEN to
# look up the firmware run.
set -euo pipefail

tag="${1:-}"
dry_run=false
case "${2:-}" in
  --dry-run) dry_run=true ;;
  "") ;;
  *) echo "usage: $0 <tag> [--dry-run]" >&2; exit 2 ;;
esac
if [[ ! "$tag" =~ ^fork-[0-9]{4}\.[0-9]{2}\.[0-9]{2}(-[0-9]+)?$ ]]; then
  echo "not a fork release tag: '$tag' (expected fork-YYYY.MM.DD or fork-YYYY.MM.DD-N)" >&2
  exit 2
fi

git fetch --quiet --no-tags origin "+refs/heads/main:refs/remotes/origin/main"
git fetch --quiet --tags origin
commit="$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}")" || {
  echo "tag $tag does not exist" >&2; exit 2; }
if ! git merge-base --is-ancestor "$commit" refs/remotes/origin/main; then
  echo "tag $tag points at $commit, which is not on main" >&2
  exit 1
fi

# The upstream commit the fork started from. Release notes for the first fork
# release start here.
fork_base_commit="${FORK_BASE_COMMIT:-13b47883a73d5070f35a3f04aec2ffa778b47e29}"

previous="$(git tag --list 'fork-[0-9]*' --merged "$commit" --sort=-creatordate | grep -vxF "$tag" | head -n 1 || true)"
start="$previous"
if [[ -z "$previous" ]]; then
  previous="fork-base"
  start="fork-base"
  if ! git rev-parse --verify --quiet "refs/tags/fork-base" >/dev/null; then
    git cat-file -e "$fork_base_commit^{commit}"
    start="$fork_base_commit"
    if $dry_run; then
      echo "[dry-run] would create tag fork-base at ${fork_base_commit:0:8}"
    else
      git tag fork-base "$fork_base_commit"
      git push --quiet origin refs/tags/fork-base
    fi
  fi
fi
echo "release $tag at $(git rev-parse --short "$commit"); notes start at $previous"

repo="${GITHUB_REPOSITORY:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/assets"

firmware_note="No firmware bundles are attached: no successful Firmware CI run on main matched this release's firmware sources."
run_id=""
while read -r id sha; do
  [[ -n "$id" ]] || continue
  if git cat-file -e "$sha^{commit}" 2>/dev/null \
      && git merge-base --is-ancestor "$sha" "$commit" \
      && git diff --quiet "$sha" "$commit" -- firmware .github/workflows/firmware-ci.yml; then
    run_id="$id"
    run_sha="$sha"
    break
  fi
done < <(gh run list --repo "$repo" --workflow firmware-ci.yml --branch main --status success \
           --limit 30 --json databaseId,headSha --jq '.[] | "\(.databaseId) \(.headSha)"')

if [[ -n "$run_id" ]]; then
  echo "firmware: Firmware CI run $run_id at $(git rev-parse --short "$run_sha")"
  if ! $dry_run; then
    gh run download "$run_id" --repo "$repo" --pattern 'esp32-csi-node-firmware-*' --dir "$work/firmware"
    for directory in "$work"/firmware/esp32-csi-node-firmware-*; do
      tar -C "$directory" -czf "$work/assets/${directory##*/}.tar.gz" .
    done
    (cd "$work/assets" && sha256sum ./*.tar.gz | sed 's# \./# #' > SHA256SUMS.txt)
  fi
  firmware_note="Firmware bundles come from [Firmware CI run $run_id](https://github.com/$repo/actions/runs/$run_id) at \`$(git rev-parse --short "$run_sha")\`. The firmware sources are unchanged between that commit and this release. Verify each archive with \`SHA256SUMS.txt\`, and pick the bundle for your board using \`firmware/esp32-csi-node/README.md\`. These are CI builds: no board has been validated by this release."
else
  echo "firmware: none attached"
fi

cat > "$work/notes.md" <<EOF
A release of [$repo](https://github.com/$repo), a fork of [ruvnet/RuView](https://github.com/ruvnet/RuView), at \`$(git rev-parse --short "$commit")\`.

- **Evidence labels.** Accuracy and performance statements in the linked pull requests are tagged MEASURED (with a reproducer), SYNTHETIC or CLAIMED. A CI build is not hardware evidence.
- **Firmware.** $firmware_note
- **Details.** Changes made before changelog fragments existed are described in \`CHANGELOG.md\` under Unreleased.

EOF

# Changelog fragments added since the previous fork tag, oldest first, if the
# release still contains them.
range="$start..$commit"
mapfile -t fragments < <(git log --reverse --diff-filter=A --name-only --format= "$range" -- 'changelog.d/*.md' \
                           | awk 'NF && $0 != "changelog.d/README.md" && !seen[$0]++')
present=()
for fragment in "${fragments[@]}"; do
  if git cat-file -e "$commit:$fragment" 2>/dev/null; then
    present+=("$fragment")
  fi
done
if (( ${#present[@]} > 0 )); then
  {
    echo "## Changes"
    echo
    for fragment in "${present[@]}"; do
      git show "$commit:$fragment"
      echo
    done
  } >> "$work/notes.md"
fi
echo "changelog fragments: ${#present[@]}"

args=("$tag" --repo "$repo" --verify-tag --title "RuView fork $tag" --notes-file "$work/notes.md"
      --generate-notes --latest)
if [[ -n "$previous" ]]; then
  args+=(--notes-start-tag "$previous")
fi
shopt -s nullglob
assets=("$work"/assets/*)
args+=("${assets[@]}")

if $dry_run; then
  echo "[dry-run] would run: gh release create ${args[*]}"
  echo "----- notes header -----"
  cat "$work/notes.md"
  exit 0
fi
gh release create "${args[@]}"

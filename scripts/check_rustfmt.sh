#!/usr/bin/env bash
# Check that every crate in v2/rustfmt-gated-crates.txt is rustfmt-clean.
#
# Usage: scripts/check_rustfmt.sh [--report]
#
# --report also lists workspace crates that are already clean but not on the
# list yet, so they can be added. Run from anywhere in the repository.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
list="$root/v2/rustfmt-gated-crates.txt"
report=false
case "${1:-}" in
  --report) report=true ;;
  "") ;;
  *) echo "usage: $0 [--report]" >&2; exit 2 ;;
esac

mapfile -t crates < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$list" | awk 'NF')
if (( ${#crates[@]} == 0 )); then
  echo "no crates listed in $list" >&2
  exit 2
fi

cd "$root/v2"
args=()
for crate in "${crates[@]}"; do
  if [[ ! "$crate" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "invalid crate name in $list: $crate" >&2
    exit 2
  fi
  args+=(-p "$crate")
done

echo "rustfmt check on ${#crates[@]} gated crates ($(rustfmt --version))"
if ! cargo fmt --check "${args[@]}"; then
  echo "::error::A gated crate is not rustfmt-clean. Run 'cargo fmt -p <crate>' in v2/ and commit the result."
  exit 1
fi
echo "all gated crates are rustfmt-clean"

if $report; then
  echo "clean workspace crates not on the list:"
  while read -r name path; do
    [[ "$path" == vendor/* ]] && continue
    printf '%s\n' "${crates[@]}" | grep -qxF "$name" && continue
    if cargo fmt --check -p "$name" >/dev/null 2>&1; then
      echo "  $name"
    fi
  done < <(cargo metadata --no-deps --format-version 1 \
             | python3 -c 'import json, sys
root = sys.argv[1]
for p in json.load(sys.stdin)["packages"]:
    print(p["name"], p["manifest_path"][len(root) + 1:])' "$PWD")
fi

#!/usr/bin/env bash
# Propose a merge of upstream RuView main into this fork's main.
#
# Usage: scripts/upstream_sync.sh [--dry-run]
#
# Run by .github/workflows/upstream-sync.yml, or by hand from a clean checkout
# of the fork. It never pushes to the base branch and never merges a pull
# request; it only proposes one.
#
#   - Upstream already merged: prints "up to date" and exits 0.
#   - Clean merge: pushes sync/upstream-<sha> with a merge commit, starts the
#     required checks on it, and opens a pull request. If the repository does
#     not let Actions open pull requests, it opens an issue with the compare
#     link instead.
#   - Conflicts: opens (or updates) one issue that lists the conflicting files.
#
# The sync pull request must be merged with a merge commit, not squashed, so
# upstream's commit ids stay in the fork's history and later syncs stay clean.
#
# Environment:
#   UPSTREAM_URL  upstream repository (default https://github.com/ruvnet/RuView.git)
#   UPSTREAM_REF  upstream branch (default main)
#   BASE          fork branch to sync into (default main)
#   GH_TOKEN      token for gh (not needed with --dry-run)
set -euo pipefail

dry_run=false
case "${1:-}" in
  --dry-run) dry_run=true ;;
  "") ;;
  *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

upstream_url="${UPSTREAM_URL:-https://github.com/ruvnet/RuView.git}"
upstream_ref="${UPSTREAM_REF:-main}"
base="${BASE:-main}"
required_checks=(csi-data-policy.yml fix-regression-guard.yml)
conflict_title="Upstream sync needs a manual merge"

# Branch names come from git, but keep them to a safe alphabet anyway.
for name in "$upstream_ref" "$base"; do
  if [[ ! "$name" =~ ^[A-Za-z0-9._/-]+$ ]]; then
    echo "refusing branch name: $name" >&2
    exit 2
  fi
done

if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
  echo "the working tree has uncommitted changes; run from a clean checkout" >&2
  exit 2
fi

git fetch --quiet --no-tags origin "+refs/heads/$base:refs/remotes/origin/$base"
git fetch --quiet --no-tags "$upstream_url" "+refs/heads/$upstream_ref:refs/remotes/upstream-sync/$upstream_ref"
up="refs/remotes/upstream-sync/$upstream_ref"
fork="refs/remotes/origin/$base"

if git merge-base --is-ancestor "$up" "$fork"; then
  echo "up to date: $base already contains upstream $(git rev-parse --short "$up")"
  exit 0
fi

short="$(git rev-parse --short=8 "$up")"
branch="sync/upstream-$short"
count="$(git rev-list --count "$fork..$up")"
commits="$(git log --no-merges --format='- %h %s' "$fork..$up" | head -n 50)"
if (( count > 50 )); then
  commits+=$'\n'"- ... and $((count - 50)) more"
fi
echo "upstream has $count commit(s) that $base lacks (upstream at $short)"

if git ls-remote --exit-code --heads origin "$branch" >/dev/null 2>&1; then
  echo "$branch already exists on origin; nothing to do"
  exit 0
fi

git -c advice.detachedHead=false checkout --quiet --detach "$fork"
author=(-c user.name="${GIT_AUTHOR_NAME:-github-actions[bot]}"
        -c user.email="${GIT_AUTHOR_EMAIL:-41898282+github-actions[bot]@users.noreply.github.com}")

if ! git "${author[@]}" merge --quiet --no-ff --no-edit \
      -m "chore(sync): merge upstream $upstream_ref at $short" "$up" >/dev/null 2>&1; then
  conflicts="$(git diff --name-only --diff-filter=U | head -n 100)"
  git merge --abort
  echo "merge conflicts:"
  echo "$conflicts"
  body="Upstream \`$upstream_ref\` is at \`$short\`, and merging it into \`$base\` conflicts.

Conflicting files:
\`\`\`
$conflicts
\`\`\`

Upstream commits that \`$base\` lacks ($count):
$commits

Resolve locally on a \`$branch\` branch: \`git merge --no-ff upstream/$upstream_ref\`, run the affected tests, then open a pull request and merge it **with a merge commit** (not a squash).

_Opened by \`scripts/upstream_sync.sh\`._"
  if $dry_run; then
    echo "[dry-run] would open or update issue: $conflict_title"
    exit 0
  fi
  existing="$(gh issue list --state open --search "in:title \"$conflict_title\"" --json number --jq '.[0].number // empty')"
  if [[ -n "$existing" ]]; then
    gh issue comment "$existing" --body "$body"
  else
    gh issue create --title "$conflict_title" --body "$body"
  fi
  exit 0
fi

echo "clean merge: $(git rev-parse --short HEAD)"
pr_body="Merges upstream \`$upstream_ref\` (\`$short\`) into \`$base\`. The merge applied cleanly.

Upstream commits ($count):
$commits

**Merge this with a merge commit, not a squash**, so upstream's commit ids stay in the fork's history and the next sync stays clean.

Before merging, run the tests for the crates these commits touch: this pull request was opened by a workflow, so only the required checks were started on it.

_Opened by \`scripts/upstream_sync.sh\`._"

if $dry_run; then
  echo "[dry-run] would push $branch and open a pull request into $base"
  exit 0
fi

git push --quiet origin "HEAD:refs/heads/$branch"
# Pushes and pull requests made with the workflow token do not start other
# workflows, but dispatches do, so start the required checks explicitly.
for workflow in "${required_checks[@]}"; do
  gh workflow run "$workflow" --ref "$branch" || echo "could not start $workflow" >&2
done

title="chore(sync): merge upstream $upstream_ref at $short"
if ! gh pr create --base "$base" --head "$branch" --title "$title" --body "$pr_body"; then
  repo="${GITHUB_REPOSITORY:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
  gh issue create --title "Upstream sync ready: $short" --body "The workflow could not open a pull request (the repository may not allow Actions to create them). The merge is ready on \`$branch\`:

https://github.com/$repo/compare/$base...$branch?expand=1

$pr_body"
fi

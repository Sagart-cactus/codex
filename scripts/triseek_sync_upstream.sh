#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

PATCH_REMOTE="${PATCH_REMOTE-origin}"
PATCH_BRANCH="${PATCH_BRANCH-codex/triseek-live-demo}"
UPSTREAM_REMOTE="${UPSTREAM_REMOTE-upstream}"
UPSTREAM_REPO="${UPSTREAM_REPO-https://github.com/openai/codex.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH-main}"
OUTPUT_BRANCH="${OUTPUT_BRANCH-codex/triseek-upstream-sync}"
VERIFY_BUILD="${VERIFY_BUILD-1}"
VERIFY_CMD="${VERIFY_CMD-cargo build -p codex-cli --bin codex-triseek --manifest-path codex-rs/Cargo.toml}"
PUSH_BRANCH="${PUSH_BRANCH-0}"
PUSH_REMOTE="${PUSH_REMOTE-origin}"

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Refusing to sync with a dirty worktree. Commit or stash your changes first." >&2
    exit 1
fi

if ! git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
    git remote add "$UPSTREAM_REMOTE" "$UPSTREAM_REPO"
fi

git fetch --no-tags "$UPSTREAM_REMOTE" "+refs/heads/$UPSTREAM_BRANCH:refs/remotes/$UPSTREAM_REMOTE/$UPSTREAM_BRANCH" --prune
upstream_ref="refs/remotes/$UPSTREAM_REMOTE/$UPSTREAM_BRANCH"

if [[ -n "$PATCH_REMOTE" ]]; then
    git fetch --no-tags "$PATCH_REMOTE" "+refs/heads/$PATCH_BRANCH:refs/remotes/$PATCH_REMOTE/$PATCH_BRANCH" --prune
    patch_ref="refs/remotes/$PATCH_REMOTE/$PATCH_BRANCH"
else
    git rev-parse --verify "$PATCH_BRANCH" >/dev/null
    patch_ref="$PATCH_BRANCH"
fi

merge_base="$(git merge-base "$patch_ref" "$upstream_ref")"
merge_commits="$(git rev-list --count --merges "${merge_base}..${patch_ref}")"
if [[ "$merge_commits" != "0" ]]; then
    cat >&2 <<EOF
Refusing to sync from $patch_ref because it contains merge commits after $merge_base.
Keep the TriSeek patch branch linear and replayable, or point PATCH_BRANCH at a linear patch branch.
EOF
    exit 1
fi

patch_commits=()
while IFS= read -r commit; do
    patch_commits+=("$commit")
done < <(git rev-list --reverse --no-merges "${merge_base}..${patch_ref}")

git switch --force-create "$OUTPUT_BRANCH" "$upstream_ref"

if [[ "${#patch_commits[@]}" -eq 0 ]]; then
    echo "No TriSeek patch commits found between $merge_base and $patch_ref." >&2
else
    for commit in "${patch_commits[@]}"; do
        echo "Cherry-picking $commit"
        if ! git cherry-pick --empty=drop -x "$commit"; then
            echo "Cherry-pick failed for $commit. Resolve the conflict and rerun." >&2
            git cherry-pick --abort || true
            exit 1
        fi
    done
fi

if [[ "$VERIFY_BUILD" == "1" ]]; then
    echo "Running verification build"
    bash -lc "$VERIFY_CMD"
fi

if [[ "$PUSH_BRANCH" == "1" ]]; then
    git fetch --no-tags "$PUSH_REMOTE" "+refs/heads/$OUTPUT_BRANCH:refs/remotes/$PUSH_REMOTE/$OUTPUT_BRANCH" --prune || true
    git push --force-with-lease "$PUSH_REMOTE" "HEAD:refs/heads/$OUTPUT_BRANCH"
fi

cat <<EOF
TriSeek upstream sync complete.
Upstream ref: $(git rev-parse --short "$upstream_ref")
Patch ref: $(git rev-parse --short "$patch_ref")
Output branch: $OUTPUT_BRANCH
Output commit: $(git rev-parse --short HEAD)
Verification build: $([[ "$VERIFY_BUILD" == "1" ]] && echo "yes" || echo "no")
Pushed: $([[ "$PUSH_BRANCH" == "1" ]] && echo "yes" || echo "no")
EOF

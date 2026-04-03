# TriSeek Upstream Sync

This repo carries a TriSeek patch stack on top of upstream Codex. The sync flow rebuilds a fresh branch from `openai/codex` and replays the TriSeek commits on top.

## Branch model

- Source patch branch: `codex/triseek-live-demo`
- Generated synced branch: `codex/triseek-upstream-sync`
- Upstream base: `openai/codex` `main`

The generated branch is recreated from upstream every run. It is not meant to be edited directly.
The source patch branch should stay linear. Do not merge `upstream/main` into it.

## Manual sync

First make sure your worktree is clean. The sync script switches branches and will refuse to run on top of local modifications.

```bash
cd /Users/trivedi/Documents/Projects/codex
./scripts/triseek_sync_upstream.sh
```

If you use `just`, there is also a wrapper:

```bash
cd /Users/trivedi/Documents/Projects/codex
just triseek-sync
```

By default this will:

- fetch `upstream/main`
- fetch `origin/codex/triseek-live-demo`
- recreate `codex/triseek-upstream-sync` from the latest upstream
- cherry-pick the TriSeek patch stack onto it
- run `cargo build -p codex-cli --bin codex-triseek --manifest-path codex-rs/Cargo.toml`

Optional environment variables:

- `PATCH_REMOTE`: defaults to `origin`
  Set `PATCH_REMOTE=''` to replay from a local branch without fetching a remote copy.
- `PATCH_BRANCH`: defaults to `codex/triseek-live-demo`
- `UPSTREAM_REMOTE`: defaults to `upstream`
- `UPSTREAM_BRANCH`: defaults to `main`
- `OUTPUT_BRANCH`: defaults to `codex/triseek-upstream-sync`
- `VERIFY_BUILD=0`: skip the build step
- `PUSH_BRANCH=1`: force-push the regenerated branch to `origin`
- `PUSH_REMOTE`: defaults to `origin`

Example:

```bash
PUSH_BRANCH=1 OUTPUT_BRANCH=codex/triseek-upstream-sync ./scripts/triseek_sync_upstream.sh
```

Equivalent `just` invocation:

```bash
just triseek-sync push_branch=1 output_branch=codex/triseek-upstream-sync
```

## GitHub Actions

The workflow is at `.github/workflows/triseek-upstream-sync.yml`.

It supports:

- manual runs via `workflow_dispatch`
- a daily scheduled run

The workflow installs Rust, fetches upstream Codex, replays the TriSeek patch stack, rebuilds `codex-triseek`, and optionally pushes the regenerated branch back to the fork.

## Notes

- Scheduled workflows only run when this workflow file exists on the repository default branch.
- If cherry-picks stop due to conflicts, resolve them on the source patch branch as ordinary follow-up commits and rerun the sync.
- The sync script refuses patch branches that contain merge commits after the upstream merge-base.
- The replay logic uses the merge-base between the source patch branch and upstream `main`, so it automatically reuses the current TriSeek patch stack rather than hard-coding commit SHAs.

# TriSeek Demo Workflow

This workflow records a side-by-side comparison between baseline Codex and
Codex with TriSeek-enabled `grep_files` routing, and it also produces a
benchmark summary for the same live prompt.

## Goal

Run the same search-intensive task in two terminals on a medium or large repo:

- baseline Codex: `grep_files` falls back to `rg`
- TriSeek Codex: `grep_files` uses a prebuilt shared index when appropriate

## Live recording

Use the live recorder when you want the demo to show Codex genuinely searching
the repository in order to solve the task.

```bash
scripts/triseek_record_live_side_by_side.sh
```

Current defaults use:

- repo: `../TriSeek-bench/repos/torvalds_linux`
- prompt: `scripts/triseek-demo/linux-sanitizer-layout.prompt.txt`
- model: `gpt-5.4`

The recorder shows:

- the exact prompt at the top of each pane
- normal `codex exec` commentary and final answer
- a parallel rollout monitor that prints each `grep_files` call plus whether it
  timed out, returned no matches, or found matching paths

Artifacts are written under `artifacts/triseek-live-demo/`.

Key outputs:

- `codex-vs-triseek-live.mp4`
- `codex-vs-triseek-live.gif`
- `live-baseline.cast`
- `live-triseek.cast`

## Live benchmark

Use the live benchmark to collect repeated runs of the same prompt with the
real model and network.

```bash
scripts/triseek_benchmark_live.sh
```

The benchmark stores each run log and final answer under
`artifacts/triseek-live-benchmark/` and writes:

- `benchmark-summary.md`
- `results.json`

Because the model and network are real, interpret the numbers as evidence for
this specific workload rather than a universal latency claim.

## Index behavior

TriSeek stores shared indexes under:

```bash
~/.codex/triseek/indexes/<repo-hash>/
```

Useful files:

- `status.json`: current build status
- `metadata.json`: index metadata once ready
- `build.lock`: present while a build is running

The live recorder and benchmark prebuild the shared index by default so the
comparison measures warm-index behavior, not background build time.

## Interpreting the result

The right story is usually:

- medium and large repos: repeated literal content searches get faster once the shared index is ready
- cold start: TriSeek may still fall back to `rg` while the index builds in the background
- small repos: no meaningful win, which is expected and already disclosed

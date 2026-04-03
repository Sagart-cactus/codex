#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_RS_ROOT="$ROOT/codex-rs"
TRISEEK_ROOT="${TRISEEK_ROOT:-$(cd "$ROOT/../TriSeek" && pwd)}"
BENCH_REPO="${BENCH_REPO:-$(cd "$ROOT/../TriSeek-bench/repos/torvalds_linux" && pwd)}"
PROMPT_FILE="${PROMPT_FILE:-$ROOT/scripts/triseek-demo/linux-sanitizer-layout.prompt.txt}"
ARTIFACT_DIR="${ARTIFACT_DIR:-$ROOT/artifacts/triseek-live-benchmark}"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex-live-demo}"
CODEX_BIN="${CODEX_BIN:-$CODEX_RS_ROOT/target/debug/codex}"
SEARCH_CLI_BIN="${SEARCH_CLI_BIN:-$TRISEEK_ROOT/target/release/search-cli}"
RUNNER="$ROOT/scripts/triseek-demo/run_live_search_case.sh"
MODEL_CATALOG_PATH="$ARTIFACT_DIR/model-catalog.json"
RUNS="${RUNS:-3}"

mkdir -p "$ARTIFACT_DIR"
mkdir -p "$CODEX_HOME"

pushd "$CODEX_RS_ROOT" >/dev/null
cargo build -p codex-cli
popd >/dev/null

if [ ! -x "$SEARCH_CLI_BIN" ]; then
  pushd "$TRISEEK_ROOT" >/dev/null
  cargo build --release -p search-cli
  popd >/dev/null
fi

python3 - "$CODEX_RS_ROOT/core/models.json" "$MODEL_CATALOG_PATH" <<'PY'
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
dest = pathlib.Path(sys.argv[2])
models = json.loads(source.read_text())["models"]
model = next(model for model in models if model["slug"] == "gpt-5.4")
model["experimental_supported_tools"] = ["grep_files"]
dest.write_text(json.dumps({"models": [model]}, indent=2))
PY

export TRISEEK_DEMO_MODEL="gpt-5.4"
export TRISEEK_DEMO_MODEL_CATALOG="$MODEL_CATALOG_PATH"

repo_real="$(cd "$BENCH_REPO" && pwd -P)"
repo_hash="$(python3 - "$repo_real" <<'PY'
import hashlib
import os
import sys
print(hashlib.sha1(os.fsencode(sys.argv[1])).hexdigest())
PY
)"
index_dir="$CODEX_HOME/triseek/indexes/$repo_hash"

if [ ! -f "$index_dir/metadata.json" ]; then
  mkdir -p "$(dirname "$index_dir")"
  "$SEARCH_CLI_BIN" build --repo "$repo_real" --index-dir "$index_dir"
fi

run_variant() {
  local variant="$1"
  local run_id="$2"
  local log_file="$ARTIFACT_DIR/${variant}-run-${run_id}.log"
  local answer_file="$ARTIFACT_DIR/${variant}-run-${run_id}.answer.txt"

  echo "running $variant #$run_id"
  TRISEEK_DEMO_OUTPUT_LAST_MESSAGE="$answer_file" \
    /usr/bin/time -lp \
    "$RUNNER" "$variant" "$repo_real" "$CODEX_HOME" "$CODEX_BIN" "$PROMPT_FILE" \
    >"$log_file" 2>&1
}

for run_id in $(seq 1 "$RUNS"); do
  run_variant baseline "$run_id"
done

for run_id in $(seq 1 "$RUNS"); do
  run_variant triseek "$run_id"
done

python3 - "$ARTIFACT_DIR" "$repo_real" "$PROMPT_FILE" "$index_dir" "$RUNS" <<'PY'
import json
import pathlib
import statistics
import sys
from datetime import datetime, timezone

artifact_dir = pathlib.Path(sys.argv[1])
repo_real = pathlib.Path(sys.argv[2])
prompt_file = pathlib.Path(sys.argv[3])
index_dir = pathlib.Path(sys.argv[4])
runs = int(sys.argv[5])

expected_answer = "\n".join(
    [
        "KASAN: Kconfig=lib/Kconfig.kasan; runtime=mm/kasan",
        "KCSAN: Kconfig=lib/Kconfig.kcsan; runtime=kernel/kcsan",
        "KFENCE: Kconfig=lib/Kconfig.kfence; runtime=mm/kfence",
        "KMSAN: Kconfig=lib/Kconfig.kmsan; runtime=mm/kmsan",
    ]
)


def parse_real_seconds(log_path: pathlib.Path) -> float:
    for line in log_path.read_text().splitlines():
        if line.startswith("real "):
            return float(line.split()[1])
    raise RuntimeError(f"missing real time in {log_path}")


def load_answer(answer_path: pathlib.Path) -> str:
    return answer_path.read_text().strip()


def summarize(series: list[float]) -> dict[str, float]:
    return {
        "mean": statistics.fmean(series),
        "min": min(series),
        "max": max(series),
        "stddev": statistics.stdev(series) if len(series) > 1 else 0.0,
    }


results = {}
verification = {}

for variant in ("baseline", "triseek"):
    times = []
    answers = []
    for run_id in range(1, runs + 1):
        log_path = artifact_dir / f"{variant}-run-{run_id}.log"
        answer_path = artifact_dir / f"{variant}-run-{run_id}.answer.txt"
        times.append(parse_real_seconds(log_path))
        answers.append(load_answer(answer_path))

    results[variant] = {
        "times": times,
        "summary": summarize(times),
    }
    verification[variant] = {
        "all_match_expected": all(answer == expected_answer for answer in answers),
        "answers": answers,
    }

speedup = results["baseline"]["summary"]["mean"] / results["triseek"]["summary"]["mean"]

json_payload = {
    "generated_at": datetime.now(timezone.utc).isoformat(),
    "repo": str(repo_real),
    "prompt_file": str(prompt_file),
    "index_dir": str(index_dir),
    "runs": runs,
    "expected_answer": expected_answer,
    "results": results,
    "verification": verification,
    "mean_speedup": speedup,
}
artifact_dir.joinpath("results.json").write_text(json.dumps(json_payload, indent=2))

def fmt(value: float) -> str:
    return f"{value:.2f}"

summary = f"""# Live TriSeek Benchmark

- Generated: {json_payload['generated_at']}
- Repo: `{repo_real}`
- Prompt: `{prompt_file}`
- Index dir: `{index_dir}`
- Runs per variant: {runs}
- Expected answer matched in every run:
  - baseline: {'yes' if verification['baseline']['all_match_expected'] else 'no'}
  - triseek: {'yes' if verification['triseek']['all_match_expected'] else 'no'}
- Scope: real `codex exec`, real model/network, warm shared index, large repo

| Variant | Mean s | Stddev s | Min s | Max s |
|---|---:|---:|---:|---:|
| baseline | {fmt(results['baseline']['summary']['mean'])} | {fmt(results['baseline']['summary']['stddev'])} | {fmt(results['baseline']['summary']['min'])} | {fmt(results['baseline']['summary']['max'])} |
| triseek | {fmt(results['triseek']['summary']['mean'])} | {fmt(results['triseek']['summary']['stddev'])} | {fmt(results['triseek']['summary']['min'])} | {fmt(results['triseek']['summary']['max'])} |

- Mean speedup: {speedup:.2f}x
- Interpretation: this prompt intentionally uses repeated literal `grep_files` searches over the large Linux tree, which is a workload TriSeek is designed to accelerate. The result is not a claim about all query shapes or all repositories.
"""
artifact_dir.joinpath("benchmark-summary.md").write_text(summary)
PY

echo "benchmark summary: $ARTIFACT_DIR/benchmark-summary.md"

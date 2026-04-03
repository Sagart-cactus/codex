#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_ROOT="$ROOT"
CODEX_RS_ROOT="$CODEX_ROOT/codex-rs"
TRISEEK_ROOT="${TRISEEK_ROOT:-$(cd "$CODEX_ROOT/../TriSeek" && pwd)}"
BENCH_REPO="${BENCH_REPO:-$(cd "$CODEX_ROOT/../TriSeek-bench/repos/torvalds_linux" && pwd)}"
PROMPT_FILE="${PROMPT_FILE:-$CODEX_ROOT/scripts/triseek-demo/linux-sanitizer-layout.prompt.txt}"
ARTIFACT_DIR="${ARTIFACT_DIR:-$CODEX_ROOT/artifacts/triseek-live-demo}"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex-live-demo}"
CODEX_BIN="${CODEX_BIN:-$CODEX_RS_ROOT/target/debug/codex}"
SEARCH_CLI_BIN="${SEARCH_CLI_BIN:-$TRISEEK_ROOT/target/release/search-cli}"
RUNNER="$CODEX_ROOT/scripts/triseek-demo/run_live_search_case.sh"
MODEL_CATALOG_PATH="$ARTIFACT_DIR/model-catalog.json"
PLAYBACK_SPEED="${PLAYBACK_SPEED:-2.0}"

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

baseline_cast="$ARTIFACT_DIR/live-baseline.cast"
triseek_cast="$ARTIFACT_DIR/live-triseek.cast"
baseline_gif="$ARTIFACT_DIR/live-baseline.gif"
triseek_gif="$ARTIFACT_DIR/live-triseek.gif"
side_by_side_mp4="$ARTIFACT_DIR/codex-vs-triseek-live.mp4"
side_by_side_gif="$ARTIFACT_DIR/codex-vs-triseek-live.gif"

rm -f \
  "$baseline_cast" \
  "$triseek_cast" \
  "$baseline_gif" \
  "$triseek_gif" \
  "$side_by_side_mp4" \
  "$side_by_side_gif"

baseline_cmd="TRISEEK_DEMO_MODEL=$TRISEEK_DEMO_MODEL TRISEEK_DEMO_MODEL_CATALOG=\"$TRISEEK_DEMO_MODEL_CATALOG\" \"$RUNNER\" baseline \"$repo_real\" \"$CODEX_HOME\" \"$CODEX_BIN\" \"$PROMPT_FILE\""
triseek_cmd="TRISEEK_DEMO_MODEL=$TRISEEK_DEMO_MODEL TRISEEK_DEMO_MODEL_CATALOG=\"$TRISEEK_DEMO_MODEL_CATALOG\" \"$RUNNER\" triseek \"$repo_real\" \"$CODEX_HOME\" \"$CODEX_BIN\" \"$PROMPT_FILE\""

asciinema rec --headless --overwrite --idle-time-limit 1 --window-size 132x40 --command "$baseline_cmd" "$baseline_cast"
asciinema rec --headless --overwrite --idle-time-limit 1 --window-size 132x40 --command "$triseek_cmd" "$triseek_cast"

agg --theme github-dark --font-size 15 --speed "$PLAYBACK_SPEED" --idle-time-limit 1 "$baseline_cast" "$baseline_gif"
agg --theme github-dark --font-size 15 --speed "$PLAYBACK_SPEED" --idle-time-limit 1 "$triseek_cast" "$triseek_gif"

ffmpeg -y \
  -ignore_loop 1 -i "$baseline_gif" \
  -ignore_loop 1 -i "$triseek_gif" \
  -filter_complex "[0:v]tpad=stop_mode=clone:stop_duration=2[left];[1:v]tpad=stop_mode=clone:stop_duration=2[right];[left][right]hstack=inputs=2,pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p[v]" \
  -map "[v]" \
  "$side_by_side_mp4"

ffmpeg -y \
  -i "$side_by_side_mp4" \
  -vf "fps=8,scale=1440:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" \
  "$side_by_side_gif"

echo "video: $side_by_side_mp4"
echo "gif:   $side_by_side_gif"

#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 5 ]; then
  echo "usage: $0 <baseline|triseek> <repo> <codex_home> <codex_bin> <prompt_file>" >&2
  exit 2
fi

mode="$1"
repo="$2"
codex_home="$3"
codex_bin="$4"
prompt_file="$5"

case "$mode" in
  baseline)
    triseek_enabled=false
    banner="BASELINE: Codex search routed to rg"
    monitor_prefix="search-rg"
    ;;
  triseek)
    triseek_enabled=true
    banner="TRISEEK: Codex search routed to indexed grep_files"
    monitor_prefix="search-ix"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac

repo_real="$(cd "$repo" && pwd -P)"
prompt_real="$(cd "$(dirname "$prompt_file")" && pwd -P)/$(basename "$prompt_file")"
model="${TRISEEK_DEMO_MODEL:-gpt-5.4}"
runner_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
streamer="$runner_dir/stream_rollout_events.py"

if [ ! -f "$prompt_real" ]; then
  echo "prompt file not found: $prompt_real" >&2
  exit 1
fi

if [ ! -x "$streamer" ]; then
  chmod +x "$streamer"
fi

if [ -z "${OPENAI_API_KEY:-}" ]; then
  echo "OPENAI_API_KEY must be set" >&2
  exit 1
fi

printf '%s\n' "$banner"
printf 'repo:  %s\n' "$repo_real"
printf 'model: %s\n' "$model"
printf '%s\n' 'prompt:'
sed 's/^/  /' "$prompt_real"
printf '\n'

extra_args=()
if [ -n "${TRISEEK_DEMO_MODEL_CATALOG:-}" ]; then
  extra_args+=(-c "model_catalog_json=\"$TRISEEK_DEMO_MODEL_CATALOG\"")
fi
if [ -n "${TRISEEK_DEMO_OUTPUT_LAST_MESSAGE:-}" ]; then
  extra_args+=(--output-last-message "$TRISEEK_DEMO_OUTPUT_LAST_MESSAGE")
fi

start_epoch="$(python3 -c 'import time; print(time.time())')"
python3 "$streamer" \
  --sessions-root "$codex_home/sessions" \
  --repo-root "$repo_real" \
  --started-at "$start_epoch" \
  --prefix "$monitor_prefix" \
  >/dev/stdout 2>/dev/stderr &
monitor_pid="$!"
trap 'kill "$monitor_pid" 2>/dev/null || true' EXIT

set +e
env -i \
  PATH="${PATH:-/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin}" \
  HOME="${HOME:-$codex_home}" \
  SHELL="${SHELL:-/bin/zsh}" \
  TERM="${TERM:-xterm-256color}" \
  LANG="${LANG:-en_US.UTF-8}" \
  LC_ALL="${LC_ALL:-en_US.UTF-8}" \
  TMPDIR="${TMPDIR:-/tmp}" \
  OPENAI_API_KEY="$OPENAI_API_KEY" \
  CODEX_HOME="$codex_home" \
  "$codex_bin" exec \
  --color never \
  --sandbox read-only \
  -C "$repo_real" \
  -m "$model" \
  -c 'approval_policy="never"' \
  -c "triseek.enabled=$triseek_enabled" \
  -c 'triseek.auto_build=false' \
  -c 'triseek.min_index_category="medium"' \
  -c "triseek.index_root=\"$codex_home/triseek/indexes\"" \
  "${extra_args[@]}" \
  < "$prompt_real"
status="$?"
set -e

sleep 1
kill "$monitor_pid" 2>/dev/null || true
wait "$monitor_pid" 2>/dev/null || true
trap - EXIT

exit "$status"

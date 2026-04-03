#!/usr/bin/env python3
import argparse
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path


POLL_INTERVAL = 0.10


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Stream grep_files activity from the newest Codex rollout file."
    )
    parser.add_argument("--sessions-root", required=True)
    parser.add_argument("--repo-root", required=True)
    parser.add_argument("--started-at", required=True, type=float)
    parser.add_argument("--prefix", default="search")
    parser.add_argument("--startup-timeout", type=float, default=30.0)
    return parser.parse_args()


def iso_to_epoch(raw: str) -> float:
    return datetime.fromisoformat(raw.replace("Z", "+00:00")).timestamp()


def format_elapsed(seconds: float) -> str:
    return f"{seconds:5.1f}s"


def shorten(text: str, width: int) -> str:
    if len(text) <= width:
        return text
    if width <= 3:
        return text[:width]
    return text[: width - 3] + "..."


def normalize_path(path_text: str, repo_root: Path) -> str:
    path_text = path_text.strip()
    try:
        path = Path(path_text)
        if path.is_absolute():
            return os.path.relpath(path, repo_root)
    except Exception:
        pass
    return path_text


def summarize_output(output: str, repo_root: Path) -> str:
    text = output.strip()
    if not text:
        return "empty output"
    if text == "No matches found.":
        return "no matches"
    if "timed out after" in text:
        return text
    if text.startswith("rg failed:"):
        return shorten(text, 92)

    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if not lines:
        return "empty output"

    paths = [normalize_path(line, repo_root) for line in lines]
    preview = ", ".join(shorten(path, 34) for path in paths[:3])
    if len(paths) == 1:
        return f"1 hit: {preview}"
    if len(paths) <= 3:
        return f"{len(paths)} hits: {preview}"
    return f"{len(paths)} hits: {preview}, +{len(paths) - 3} more"


def find_rollout(sessions_root: Path, started_at: float, timeout: float) -> Path | None:
    deadline = time.time() + timeout
    best: Path | None = None
    best_mtime = -1.0

    while time.time() < deadline:
        for candidate in sessions_root.rglob("rollout-*.jsonl"):
            try:
                mtime = candidate.stat().st_mtime
            except OSError:
                continue
            if mtime + 1.0 < started_at:
                continue
            if mtime > best_mtime:
                best = candidate
                best_mtime = mtime
        if best is not None:
            return best
        time.sleep(POLL_INTERVAL)

    return None


def print_line(prefix: str, message: str) -> None:
    print(f"[{prefix}] {message}", flush=True)


def main() -> int:
    args = parse_args()
    sessions_root = Path(args.sessions_root)
    repo_root = Path(args.repo_root)

    rollout = find_rollout(sessions_root, args.started_at, args.startup_timeout)
    if rollout is None:
        print_line(args.prefix, "no rollout file appeared")
        return 1

    print_line(args.prefix, f"attached to {rollout.name}")

    calls: dict[str, dict[str, object]] = {}
    sequence = 0

    with rollout.open("r", encoding="utf-8") as handle:
        while True:
            line = handle.readline()
            if not line:
                time.sleep(POLL_INTERVAL)
                continue

            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue

            payload = event.get("payload", {})
            if event.get("type") != "response_item":
                continue

            item_type = payload.get("type")
            if item_type == "function_call" and payload.get("name") == "grep_files":
                sequence += 1
                call_id = payload.get("call_id")
                if not call_id:
                    continue

                try:
                    arguments = json.loads(payload.get("arguments", "{}"))
                except json.JSONDecodeError:
                    arguments = {}

                timestamp = iso_to_epoch(event["timestamp"])
                call = {
                    "index": sequence,
                    "timestamp": timestamp,
                    "pattern": str(arguments.get("pattern", "")),
                    "path": str(arguments.get("path", ".")),
                    "include": arguments.get("include"),
                    "limit": arguments.get("limit"),
                }
                calls[call_id] = call

                include = call["include"] if call["include"] else "-"
                limit = call["limit"] if call["limit"] is not None else "-"
                pattern = shorten(call["pattern"], 52)
                path = shorten(call["path"], 32)
                print_line(
                    args.prefix,
                    f"#{sequence:02d} +{format_elapsed(timestamp - args.started_at)} grep_files pattern={pattern!r} path={path} include={include} limit={limit}",
                )
            elif item_type == "function_call_output":
                call_id = payload.get("call_id")
                call = calls.get(call_id)
                if call is None:
                    continue

                timestamp = iso_to_epoch(event["timestamp"])
                duration = timestamp - float(call["timestamp"])
                summary = summarize_output(str(payload.get("output", "")), repo_root)
                print_line(
                    args.prefix,
                    f"#{int(call['index']):02d} +{format_elapsed(timestamp - args.started_at)} done in {duration:4.1f}s -> {summary}",
                )


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(0)

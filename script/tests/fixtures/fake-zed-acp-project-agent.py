#!/usr/bin/env python3
"""Deterministic ACP fixture for the project-aware Zed canary."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def emit(message: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def session_update(update: dict[str, Any]) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "fixture-session", "update": update},
    }


def parse_marker(prompt: str) -> str:
    match = re.search(r"ZED_ACP_CANARY:[0-9a-f]+", prompt)
    if match is None:
        raise RuntimeError("canary marker is absent")
    return match.group(0)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--mode",
        choices=(
            "pass",
            "pass-without-close",
            "split-evidence",
            "prompt-echo",
            "marker-only",
            "wrong-cwd",
            "authentication",
            "capacity",
            "session-limit",
            "permission-write",
            "permission-shell",
            "permission-unknown",
            "timeout",
        ),
        required=True,
    )
    parser.add_argument("--child-pid", type=Path)
    args = parser.parse_args()

    session_cwd = ""
    for raw in sys.stdin:
        message = json.loads(raw)
        method = message.get("method")
        request_id = message.get("id")
        if method == "initialize":
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": (
                            {}
                            if args.mode == "pass-without-close"
                            else {"sessionCapabilities": {"close": {}}}
                        ),
                        "agentInfo": {"name": "fixture", "version": "1"},
                        "authMethods": [],
                    },
                }
            )
        elif method == "session/new":
            session_cwd = message.get("params", {}).get("cwd", "")
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"sessionId": "fixture-session"},
                }
            )
        elif method == "session/prompt":
            prompt = " ".join(
                item.get("text", "")
                for item in message.get("params", {}).get("prompt", [])
                if isinstance(item, dict) and item.get("type") == "text"
            )
            marker = parse_marker(prompt)
            if args.mode == "authentication":
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32001, "message": "authentication expired"},
                    }
                )
                continue
            if args.mode == "capacity":
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32002, "message": "provider capacity exhausted"},
                    }
                )
                continue
            if args.mode == "session-limit":
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {
                            "code": -32603,
                            "message": "Internal error: You've hit your session limit",
                        },
                    }
                )
                continue
            if args.mode == "timeout":
                if args.child_pid is None:
                    raise RuntimeError("timeout mode requires --child-pid")
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                child = subprocess.Popen(
                    [
                        sys.executable,
                        "-c",
                        "import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(3600)",
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    close_fds=True,
                )
                args.child_pid.write_text(f"{child.pid}\n", encoding="utf-8")
                time.sleep(3600)
                continue

            if args.mode.startswith("permission-"):
                kind = args.mode.removeprefix("permission-")
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": 99,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "fixture-session",
                            "toolCall": {
                                "toolCallId": "unsafe-tool",
                                "title": f"fixture {kind} request",
                                "kind": kind,
                            },
                            "options": [
                                {"optionId": "allow", "kind": "allow_once"}
                            ],
                        },
                    }
                )
                return 0

            observed_cwd = "/wrong/project" if args.mode == "wrong-cwd" else session_cwd
            if args.mode != "marker-only":
                if args.mode == "prompt-echo":
                    sentinel_sha = hashlib.sha256(prompt.encode()).hexdigest()
                else:
                    sentinel_sha = hashlib.sha256(
                        (Path(session_cwd) / "sentinel.txt").read_bytes()
                    ).hexdigest()
                emit(
                    session_update(
                        {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "tool-1",
                            "title": "Read the project sentinel",
                            "kind": "read",
                            "status": "in_progress",
                            "rawInput": {"path": "sentinel.txt", "expectedCwd": session_cwd},
                        }
                    )
                )
                if args.mode == "split-evidence":
                    emit(
                        session_update(
                            {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "tool-2",
                                "title": "Hash the project sentinel",
                                "kind": "read",
                                "status": "in_progress",
                                "rawInput": {"path": "sentinel.txt"},
                            }
                        )
                    )
                emit(
                    session_update(
                        {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": "tool-1",
                            "status": "completed",
                            "rawOutput": (
                                f"{observed_cwd}\n"
                                if args.mode == "split-evidence"
                                else f"{observed_cwd}\n{sentinel_sha}\n"
                            ),
                        }
                    )
                )
                if args.mode == "split-evidence":
                    emit(
                        session_update(
                            {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "tool-2",
                                "status": "completed",
                                "rawOutput": f"{sentinel_sha}\n",
                            }
                        )
                    )
            emit(
                session_update(
                    {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": marker},
                    }
                )
            )
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {"stopReason": "end_turn"},
                }
            )
        elif method == "session/close":
            if args.mode == "pass-without-close":
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32601, "message": "method not found"},
                    }
                )
            else:
                emit({"jsonrpc": "2.0", "id": request_id, "result": {}})
        elif request_id is not None:
            emit(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "method not found"},
                }
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

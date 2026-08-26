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
            "wrong-cwd-close-error",
            "authentication",
            "authentication-message",
            "capacity",
            "session-limit",
            "permission-write",
            "permission-shell",
            "permission-unknown",
            "client-read",
            "client-read-outside",
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
            if args.mode == "authentication-message":
                emit(
                    session_update(
                        {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {
                                "type": "text",
                                "text": "Please login before using this route.",
                            },
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

            if args.mode in {"client-read", "client-read-outside"}:
                requested_path = (
                    "/etc/hosts"
                    if args.mode == "client-read-outside"
                    else str(Path(session_cwd) / "sentinel.txt")
                )
                emit(
                    {
                        "jsonrpc": "2.0",
                        "id": 98,
                        "method": "fs/read_text_file",
                        "params": {
                            "sessionId": "fixture-session",
                            "path": requested_path,
                        },
                    }
                )
                response = json.loads(sys.stdin.readline())
                content = response.get("result", {}).get("content", "")
                sentinel_sha = hashlib.sha256(content.encode()).hexdigest()
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
                        "result": {
                            "stopReason": (
                                "end_turn" if sentinel_sha else "refusal"
                            )
                        },
                    }
                )
                continue

            sentinel_path = Path(session_cwd) / "sentinel.txt"
            sentinel_content = sentinel_path.read_text(encoding="utf-8")
            observed_path = (
                "/wrong/project/sentinel.txt"
                if args.mode in {"wrong-cwd", "wrong-cwd-close-error"}
                else "sentinel.txt"
            )
            if args.mode != "marker-only":
                emit(
                    session_update(
                        {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "tool-1",
                            "title": "Read the project sentinel",
                            "kind": "read",
                            "status": "in_progress",
                            "rawInput": {"path": observed_path},
                        }
                    )
                )
                if args.mode == "split-evidence":
                    emit(
                        session_update(
                            {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "tool-2",
                            "title": "Read unrelated output",
                                "kind": "read",
                                "status": "in_progress",
                            "rawInput": {"path": "not-sentinel.txt"},
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
                                "path observed\n"
                                if args.mode == "split-evidence"
                                else prompt
                                if args.mode == "prompt-echo"
                                else sentinel_content
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
                                "rawOutput": sentinel_content,
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
            if args.mode in {"pass-without-close", "wrong-cwd-close-error"}:
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

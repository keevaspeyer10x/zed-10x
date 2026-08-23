#!/usr/bin/env python3
"""Run a privacy-safe, project-aware ACP journey against one Zed route."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import queue
import re
import signal
import stat
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


SCHEMA = "zed-acp-project-canary-v1"
INITIALIZE = {
    "jsonrpc": "2.0",
    "id": 0,
    "method": "initialize",
    "params": {
        "protocolVersion": 1,
        "clientCapabilities": {},
        "clientInfo": {"name": "zed-acp-project-canary", "version": "1.0.0"},
    },
}


class CanaryFailure(RuntimeError):
    def __init__(self, failure_class: str) -> None:
        super().__init__(failure_class)
        self.failure_class = failure_class


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--surface", required=True)
    parser.add_argument("--cwd", required=True, type=Path)
    parser.add_argument("--sentinel", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--timeout-seconds", type=float, default=180)
    parser.add_argument("--termination-grace-seconds", type=float, default=5)
    parser.add_argument("--settings", type=Path)
    parser.add_argument("--endpoint")
    parser.add_argument("--command")
    parser.add_argument("--arg", action="append", default=[])
    return parser.parse_args()


def canonical_bytes(value: dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def write_exclusive(path: Path, value: dict[str, Any]) -> None:
    data = canonical_bytes(value)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.fchmod(fd, 0o600)
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise OSError("terminal evidence write made no progress")
            view = view[written:]
        os.fsync(fd)
    finally:
        os.close(fd)
    directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_text(value: str) -> str:
    return sha256_bytes(value.encode())


def read_sentinel(root: Path, relative: Path) -> tuple[Path, str]:
    if relative.is_absolute() or ".." in relative.parts or relative == Path("."):
        raise CanaryFailure("invalid_sentinel_path")
    sentinel = (root / relative).resolve(strict=True)
    if not sentinel.is_relative_to(root):
        raise CanaryFailure("invalid_sentinel_path")
    fd = os.open(sentinel, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 8 * 1024 * 1024:
            raise CanaryFailure("invalid_sentinel_file")
        digest = hashlib.sha256()
        while True:
            chunk = os.read(fd, 64 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    finally:
        os.close(fd)
    return sentinel, digest.hexdigest()


def strip_jsonc(source: str) -> str:
    output: list[str] = []
    index = 0
    in_string = False
    escaped = False
    in_line_comment = False
    in_block_comment = False
    while index < len(source):
        char = source[index]
        following = source[index + 1] if index + 1 < len(source) else ""
        if in_line_comment:
            if char == "\n":
                in_line_comment = False
                output.append(char)
            else:
                output.append(" ")
            index += 1
            continue
        if in_block_comment:
            if char == "*" and following == "/":
                output.extend((" ", " "))
                index += 2
                in_block_comment = False
            else:
                output.append("\n" if char == "\n" else " ")
                index += 1
            continue
        if in_string:
            output.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
            output.append(char)
            index += 1
        elif char == "/" and following == "/":
            output.extend((" ", " "))
            index += 2
            in_line_comment = True
        elif char == "/" and following == "*":
            output.extend((" ", " "))
            index += 2
            in_block_comment = True
        else:
            output.append(char)
            index += 1
    if in_block_comment or in_string:
        raise CanaryFailure("invalid_settings_jsonc")

    stripped = "".join(output)
    output = []
    index = 0
    in_string = False
    escaped = False
    while index < len(stripped):
        char = stripped[index]
        if in_string:
            output.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
            output.append(char)
            index += 1
            continue
        if char == ",":
            lookahead = index + 1
            while lookahead < len(stripped) and stripped[lookahead].isspace():
                lookahead += 1
            if lookahead < len(stripped) and stripped[lookahead] in "}]":
                index += 1
                continue
        output.append(char)
        index += 1
    return "".join(output)


def load_command(args: argparse.Namespace) -> tuple[str, list[str], str]:
    if args.command is not None:
        if args.settings is not None or args.endpoint is not None:
            raise CanaryFailure("ambiguous_command_source")
        command = args.command
        argv = list(args.arg)
        endpoint = "direct"
    else:
        if args.settings is None or not args.endpoint or args.arg:
            raise CanaryFailure("invalid_command_source")
        try:
            settings = json.loads(strip_jsonc(args.settings.read_text(encoding="utf-8")))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise CanaryFailure("invalid_settings_jsonc") from exc
        entry = settings.get("agent_servers", {}).get(args.endpoint)
        if not isinstance(entry, dict) or entry.get("type") != "custom":
            raise CanaryFailure("endpoint_not_configured")
        command = entry.get("command")
        argv = entry.get("args")
        if not isinstance(command, str) or not isinstance(argv, list):
            raise CanaryFailure("invalid_endpoint_command")
        if not all(isinstance(item, str) for item in argv):
            raise CanaryFailure("invalid_endpoint_command")
        endpoint = args.endpoint
    if not Path(command).is_absolute():
        raise CanaryFailure("command_not_absolute")
    return command, argv, endpoint


def classify_error(message: str) -> str:
    lowered = message.casefold()
    rules = (
        ("missing_executable", r"no such file|not found|exit (status: )?127"),
        ("authentication_expired", r"expired|token[^\n]*invalid"),
        ("authentication_required", r"auth|login|credential|unauthor"),
        ("capacity_or_rate_limit", r"capacity|rate.?limit|quota|overloaded|spend"),
        ("unsupported_route", r"unsupported|not supported|unknown model"),
        ("permission_denied", r"permission|forbidden|denied"),
        ("timeout", r"timed? ?out|timeout|deadline"),
    )
    for label, pattern in rules:
        if re.search(pattern, lowered):
            return label
    return "provider_or_transport_error"


class Transport:
    def __init__(self, command: str, argv: list[str], cwd: Path) -> None:
        self.messages: queue.Queue[dict[str, Any] | None] = queue.Queue()
        self.stderr_sha256 = hashlib.sha256()
        self.stderr_bytes = 0
        self.stderr_classes: set[str] = set()
        try:
            self.process = subprocess.Popen(
                [command, *argv],
                cwd=cwd,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                bufsize=0,
            )
        except FileNotFoundError as exc:
            raise CanaryFailure("missing_executable") from exc
        self.process_group = os.getpgid(self.process.pid)
        self.stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for raw in self.process.stdout:
            try:
                message = json.loads(raw)
            except (json.JSONDecodeError, UnicodeDecodeError):
                self.messages.put({"_malformed": True})
                continue
            self.messages.put(message if isinstance(message, dict) else {"_malformed": True})
        self.messages.put(None)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for raw in iter(lambda: self.process.stderr.read(4096), b""):
            self.stderr_sha256.update(raw)
            self.stderr_bytes += len(raw)
            failure_class = classify_error(raw.decode("utf-8", errors="replace")[:4096])
            if failure_class != "provider_or_transport_error":
                self.stderr_classes.add(failure_class)

    def send(self, message: dict[str, Any]) -> None:
        if self.process.poll() is not None or self.process.stdin is None:
            raise CanaryFailure("transport_exited")
        self.process.stdin.write(canonical_bytes(message))
        self.process.stdin.flush()

    def receive(self, deadline: float) -> dict[str, Any]:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise CanaryFailure("timeout")
        try:
            message = self.messages.get(timeout=remaining)
        except queue.Empty as exc:
            raise CanaryFailure("timeout") from exc
        if message is None:
            raise CanaryFailure("transport_exited")
        if message.get("_malformed"):
            raise CanaryFailure("malformed_acp_message")
        return message

    def group_is_gone(self) -> bool:
        try:
            os.killpg(self.process_group, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
        return False

    def finish(self, grace_seconds: float) -> bool:
        if self.process.stdin is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        try:
            self.process.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            pass
        if not self.group_is_gone():
            try:
                os.killpg(self.process_group, signal.SIGTERM)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + grace_seconds
            while time.monotonic() < deadline and not self.group_is_gone():
                time.sleep(0.02)
        if not self.group_is_gone():
            try:
                os.killpg(self.process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            self.process.wait(timeout=max(grace_seconds, 1))
        except subprocess.TimeoutExpired:
            return False
        deadline = time.monotonic() + max(grace_seconds, 1)
        while time.monotonic() < deadline:
            if self.group_is_gone():
                break
            time.sleep(0.02)
        self.stdout_thread.join(timeout=1)
        self.stderr_thread.join(timeout=1)
        return self.group_is_gone()

    def evidence(self) -> dict[str, Any]:
        return {
            "exitCode": self.process.poll(),
            "stderrBytes": self.stderr_bytes,
            "stderrSha256": self.stderr_sha256.hexdigest(),
            "stderrClassifications": sorted(self.stderr_classes),
        }


class Journey:
    def __init__(
        self,
        transport: Transport,
        cwd: Path,
        sentinel: Path,
        sentinel_sha256: str,
        timeout_seconds: float,
    ) -> None:
        self.transport = transport
        self.cwd = cwd
        self.sentinel = sentinel
        self.sentinel_sha256 = sentinel_sha256
        self.deadline = time.monotonic() + timeout_seconds
        self.nonce = os.urandom(16).hex()
        self.cwd_sha256 = sha256_text(str(cwd))
        # Keep the filesystem oracle out of the prompt. The agent must obtain the
        # sentinel digest from the project instead of echoing an expected value.
        self.marker = f"ZED_ACP_CANARY:{self.nonce}"
        self.session_id: str | None = None
        self.agent_text = ""
        self.tool_calls: dict[str, dict[str, Any]] = {}
        self.permission_requests = 0

    def observe(self, message: dict[str, Any]) -> None:
        if message.get("method") != "session/update":
            return
        update = message.get("params", {}).get("update")
        if not isinstance(update, dict):
            return
        update_type = update.get("sessionUpdate")
        if update_type == "agent_message_chunk":
            content = update.get("content")
            if isinstance(content, dict) and isinstance(content.get("text"), str):
                self.agent_text = (self.agent_text + content["text"])[-16384:]
        elif update_type in {"tool_call", "tool_call_update"}:
            tool_call_id = update.get("toolCallId")
            if isinstance(tool_call_id, str) and tool_call_id:
                fields = self.tool_calls.setdefault(tool_call_id, {})
                fields.update({key: value for key, value in update.items() if key != "sessionUpdate"})

    def reject_permission(self, _message: dict[str, Any]) -> None:
        # This canary is evidence collection, not an authorization surface. A
        # route that cannot perform a read without additional permission is
        # non-green; never infer safety from an allow_once label supplied by it.
        self.permission_requests += 1
        raise CanaryFailure("permission_requested")

    def await_response(self, request_id: int) -> dict[str, Any]:
        while True:
            message = self.transport.receive(self.deadline)
            self.observe(message)
            if message.get("method") == "session/request_permission":
                self.reject_permission(message)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                error = message.get("error")
                safe_message = error.get("message", "") if isinstance(error, dict) else ""
                raise CanaryFailure(classify_error(str(safe_message)))
            if "result" in message:
                return message

    def run(self) -> dict[str, Any]:
        self.transport.send(INITIALIZE)
        initialized = self.await_response(0)
        if initialized.get("result", {}).get("protocolVersion") != 1:
            raise CanaryFailure("unsupported_protocol")

        self.transport.send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "session/new",
                "params": {"cwd": str(self.cwd), "mcpServers": []},
            }
        )
        created = self.await_response(1)
        session_id = created.get("result", {}).get("sessionId")
        if not isinstance(session_id, str) or not session_id:
            raise CanaryFailure("invalid_session")
        self.session_id = session_id

        relative_sentinel = self.sentinel.relative_to(self.cwd)
        prompt = (
            "Use a project filesystem or terminal tool to inspect the current workspace. "
            f"Confirm the exact working directory is {json.dumps(str(self.cwd))}, then read "
            f"{json.dumps(str(relative_sentinel))} without modifying anything and compute its SHA-256. "
            "The completed tool evidence must contain the observed working directory and computed digest. "
            f"After the tool completes, reply with exactly {self.marker}"
        )
        self.transport.send(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": prompt}],
                },
            }
        )
        prompt_result = self.await_response(2)
        stop_reason = prompt_result.get("result", {}).get("stopReason")
        if not isinstance(stop_reason, str) or not stop_reason:
            raise CanaryFailure("nonterminal_response")

        completed = [
            fields
            for fields in self.tool_calls.values()
            if fields.get("status") == "completed"
        ]
        if not completed:
            raise CanaryFailure("tool_evidence_missing")
        evidence_matches = any(
            str(self.cwd)
            in json.dumps(
                {"content": fields.get("content"), "rawOutput": fields.get("rawOutput")},
                sort_keys=True,
            )
            and self.sentinel_sha256
            in json.dumps(
                {"content": fields.get("content"), "rawOutput": fields.get("rawOutput")},
                sort_keys=True,
            )
            for fields in completed
        )
        if not evidence_matches:
            raise CanaryFailure("project_evidence_mismatch")
        if self.marker not in self.agent_text:
            raise CanaryFailure("terminal_marker_missing")

        self.transport.send(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/close",
                "params": {"sessionId": session_id},
            }
        )
        self.await_response(3)
        return {
            "toolCallStarted": bool(self.tool_calls),
            "toolCallCompleted": True,
            "toolEvidenceMatched": True,
            "terminalMarkerObserved": True,
            "stopReason": stop_reason,
            "permissionRequestsObserved": self.permission_requests,
            "permissionRequestsApproved": 0,
        }


def main() -> int:
    args = parse_args()
    started = time.monotonic()
    transport: Transport | None = None
    journey: Journey | None = None
    failure_class: str | None = None
    journey_evidence: dict[str, Any] = {}
    command = ""
    argv: list[str] = []
    endpoint = args.endpoint or "direct"
    cwd: Path | None = None
    sentinel: Path | None = None
    sentinel_sha256 = ""
    process_group_gone = True
    try:
        if not 0.5 <= args.timeout_seconds <= 900:
            raise CanaryFailure("invalid_timeout")
        if not 0.05 <= args.termination_grace_seconds <= 30:
            raise CanaryFailure("invalid_termination_grace")
        if not args.output.parent.is_dir():
            raise CanaryFailure("output_parent_missing")
        cwd = args.cwd.resolve(strict=True)
        if not cwd.is_dir():
            raise CanaryFailure("invalid_project_directory")
        sentinel, sentinel_sha256 = read_sentinel(cwd, args.sentinel)
        command, argv, endpoint = load_command(args)
        transport = Transport(command, argv, cwd)
        journey = Journey(
            transport,
            cwd,
            sentinel,
            sentinel_sha256,
            args.timeout_seconds,
        )
        journey_evidence = journey.run()
    except CanaryFailure as exc:
        failure_class = exc.failure_class
    except (OSError, ValueError) as exc:
        failure_class = classify_error(str(exc))
    finally:
        if transport is not None:
            process_group_gone = transport.finish(args.termination_grace_seconds)
            if failure_class in {"transport_exited", "provider_or_transport_error"}:
                classes = transport.evidence()["stderrClassifications"]
                if classes:
                    failure_class = classes[0]
            if not process_group_gone and failure_class is None:
                failure_class = "process_cleanup_failed"

    status = "pass" if failure_class is None else "failed"
    receipt: dict[str, Any] = {
        "schema": SCHEMA,
        "status": status,
        "surface": args.surface,
        "endpoint": endpoint,
        "cwdSha256": sha256_text(str(cwd)) if cwd is not None else None,
        "sentinelPathSha256": (
            sha256_text(str(sentinel.relative_to(cwd)))
            if sentinel is not None and cwd is not None
            else None
        ),
        "sentinelSha256": sentinel_sha256 or None,
        "commandArgvSha256": (
            sha256_bytes(b"\0".join(item.encode() for item in [command, *argv]))
            if command
            else None
        ),
        "processStarted": transport is not None,
        "processGroupGone": process_group_gone,
        "toolCallStarted": journey_evidence.get("toolCallStarted", bool(journey and journey.tool_calls)),
        "toolCallCompleted": journey_evidence.get("toolCallCompleted", False),
        "toolEvidenceMatched": journey_evidence.get("toolEvidenceMatched", False),
        "terminalMarkerObserved": journey_evidence.get("terminalMarkerObserved", False),
        "permissionRequestsObserved": journey_evidence.get(
            "permissionRequestsObserved", journey.permission_requests if journey else 0
        ),
        "permissionRequestsApproved": journey_evidence.get("permissionRequestsApproved", 0),
        "stopReason": journey_evidence.get("stopReason"),
        "failureClass": failure_class,
        "promptOrResponseContentRetained": False,
        "elapsedMs": round((time.monotonic() - started) * 1000),
        "transport": transport.evidence() if transport is not None else None,
    }
    try:
        write_exclusive(args.output, receipt)
    except FileExistsError:
        print("canary output already exists", file=sys.stderr)
        return 2
    print(json.dumps(receipt, sort_keys=True))
    return 0 if status == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Run a privacy-safe, project-aware ACP journey against one Zed route."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
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
        "clientCapabilities": {
            "fs": {"readTextFile": True, "writeTextFile": False},
            "terminal": False,
        },
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
    parser.add_argument("--registry-cache", type=Path)
    parser.add_argument("--npm-command", type=Path)
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


def read_sentinel(root: Path, relative: Path) -> tuple[Path, str, str]:
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
        chunks: list[bytes] = []
        total = 0
        while True:
            chunk = os.read(fd, 64 * 1024)
            if not chunk:
                break
            total += len(chunk)
            if total > 8 * 1024 * 1024:
                raise CanaryFailure("invalid_sentinel_file")
            chunks.append(chunk)
    finally:
        os.close(fd)
    content_bytes = b"".join(chunks)
    try:
        content = content_bytes.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise CanaryFailure("invalid_sentinel_file") from exc
    return sentinel, content, sha256_bytes(content_bytes)


def nested_strings(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        return [item for child in value for item in nested_strings(child)]
    if isinstance(value, dict):
        return [item for child in value.values() for item in nested_strings(child)]
    return []


def tool_input_names_sentinel(value: Any, absolute: Path, relative: Path) -> bool:
    absolute_text = str(absolute)
    relative_text = str(relative)
    relative_pattern = re.compile(
        rf"(?<![A-Za-z0-9_./-])(?:\./)?{re.escape(relative_text)}(?![A-Za-z0-9_./-])"
    )
    for text in nested_strings(value):
        if text in {absolute_text, relative_text, f"./{relative_text}"}:
            return True
        if absolute_text in text or relative_pattern.search(text):
            return True
    return False


def rendered_sentinel_variants(sentinel_content: str) -> tuple[tuple[str, str], ...]:
    if not sentinel_content:
        return ()
    lines = sentinel_content.splitlines(keepends=True)
    return (
        ("exact", sentinel_content),
        (
            "numbered_tab",
            "".join(
                f"{line_number:>6}\t{line}"
                for line_number, line in enumerate(lines, start=1)
            ),
        ),
        (
            "numbered_tab_compact",
            "".join(
                f"{line_number}\t{line}"
                for line_number, line in enumerate(lines, start=1)
            ),
        ),
        (
            "numbered_arrow",
            "".join(
                f"{line_number:>6}→{line}"
                for line_number, line in enumerate(lines, start=1)
            ),
        ),
        (
            "numbered_arrow_compact",
            "".join(
                f"{line_number}→{line}"
                for line_number, line in enumerate(lines, start=1)
            ),
        ),
    )


def tool_output_sentinel_format(value: Any, sentinel_content: str) -> str | None:
    strings = nested_strings(value)
    for format_name, rendered in rendered_sentinel_variants(sentinel_content):
        if any(rendered in text for text in strings):
            return format_name
    return None


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


def platform_key() -> str:
    os_name = {"darwin": "darwin", "linux": "linux", "win32": "windows"}.get(
        sys.platform
    )
    architecture = {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "x86_64": "x86_64",
        "amd64": "x86_64",
    }.get(platform.machine().casefold())
    if os_name is None or architecture is None:
        raise CanaryFailure("unsupported_registry_platform")
    return f"{os_name}-{architecture}"


def sanitize_path_component(value: str) -> str:
    sanitized = "".join(
        character if character.isascii() and (character.isalnum() or character in "._-") else "-"
        for character in value
    )
    return sanitized or "unknown"


def bounded_npm_package_spec(package: str) -> str:
    package_name, separator, version = package.rpartition("@")
    if not separator or not package_name or not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        return package
    return f"{package_name}@0.0.0 - {version}"


def versioned_archive_cache_dir(
    base_dir: Path, version: str, archive_url: str, expected_sha256: str | None
) -> Path:
    sanitized_version = sanitize_path_component(version)
    version_hash = sha256_text(version)[:16]
    archive_digest = hashlib.sha256(archive_url.encode())
    if expected_sha256:
        archive_digest.update(b"\0sha256:")
        archive_digest.update(expected_sha256.casefold().encode())
    return base_dir / f"v_{sanitized_version}_{version_hash}_{archive_digest.hexdigest()[:16]}"


def load_registry_command(
    args: argparse.Namespace, endpoint: str, entry: dict[str, Any]
) -> tuple[str, list[str], dict[str, str]]:
    if args.registry_cache is None:
        raise CanaryFailure("registry_cache_required")
    registry_file = args.registry_cache / "registry.json"
    try:
        registry = json.loads(registry_file.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CanaryFailure("invalid_registry_cache") from exc
    agents = registry.get("agents") if isinstance(registry, dict) else None
    if not isinstance(agents, list):
        raise CanaryFailure("invalid_registry_cache")
    matches = [agent for agent in agents if isinstance(agent, dict) and agent.get("id") == endpoint]
    if len(matches) != 1:
        raise CanaryFailure("registry_agent_not_found")
    agent = matches[0]
    version = agent.get("version")
    distribution = agent.get("distribution")
    if not isinstance(version, str) or not isinstance(distribution, dict):
        raise CanaryFailure("invalid_registry_agent")
    settings_env = entry.get("env", {})
    if not isinstance(settings_env, dict) or not all(
        isinstance(key, str) and isinstance(value, str)
        for key, value in settings_env.items()
    ):
        raise CanaryFailure("invalid_endpoint_environment")

    binary = distribution.get("binary")
    npx = distribution.get("npx")
    if isinstance(binary, dict) and npx is None:
        target = binary.get(platform_key())
        if not isinstance(target, dict):
            raise CanaryFailure("unsupported_registry_platform")
        archive = target.get("archive")
        relative_command = target.get("cmd")
        target_args = target.get("args", [])
        expected_sha256 = target.get("sha256")
        distribution_env = target.get("env", {})
        if (
            not isinstance(archive, str)
            or not isinstance(relative_command, str)
            or not relative_command.startswith(("./", ".\\"))
            or ".." in Path(relative_command[2:]).parts
            or not isinstance(target_args, list)
            or not all(isinstance(item, str) for item in target_args)
            or (expected_sha256 is not None and not isinstance(expected_sha256, str))
            or not isinstance(distribution_env, dict)
            or not all(
                isinstance(key, str) and isinstance(value, str)
                for key, value in distribution_env.items()
            )
        ):
            raise CanaryFailure("invalid_registry_agent")
        version_dir = versioned_archive_cache_dir(
            args.registry_cache / endpoint, version, archive, expected_sha256
        )
        command_path = version_dir / relative_command[2:]
        if not command_path.is_file():
            raise CanaryFailure("registry_artifact_not_installed")
        environment = {**distribution_env, **settings_env}
        return str(command_path), target_args, environment

    if isinstance(npx, dict) and binary is None:
        package = npx.get("package")
        npx_args = npx.get("args", [])
        distribution_env = npx.get("env", {})
        if (
            args.npm_command is None
            or not args.npm_command.is_absolute()
            or not args.npm_command.is_file()
            or not isinstance(package, str)
            or not isinstance(npx_args, list)
            or not all(isinstance(item, str) for item in npx_args)
            or not isinstance(distribution_env, dict)
            or not all(
                isinstance(key, str) and isinstance(value, str)
                for key, value in distribution_env.items()
            )
        ):
            raise CanaryFailure("invalid_registry_agent")
        prefix = args.registry_cache / "npx" / sanitize_path_component(endpoint)
        prefix.mkdir(mode=0o700, parents=True, exist_ok=True)
        command_args = [
            "--prefix",
            str(prefix),
            "exec",
            "--yes",
            "--",
            bounded_npm_package_spec(package),
            *npx_args,
        ]
        environment = {**distribution_env, **settings_env}
        return str(args.npm_command), command_args, environment

    raise CanaryFailure("invalid_registry_agent")


def load_command(args: argparse.Namespace) -> tuple[str, list[str], str, dict[str, str]]:
    if args.command is not None:
        if args.settings is not None or args.endpoint is not None:
            raise CanaryFailure("ambiguous_command_source")
        command = args.command
        argv = list(args.arg)
        endpoint = "direct"
        environment: dict[str, str] = {}
    else:
        if args.settings is None or not args.endpoint or args.arg:
            raise CanaryFailure("invalid_command_source")
        try:
            settings = json.loads(strip_jsonc(args.settings.read_text(encoding="utf-8")))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise CanaryFailure("invalid_settings_jsonc") from exc
        entry = settings.get("agent_servers", {}).get(args.endpoint)
        if not isinstance(entry, dict) or entry.get("type") not in {"custom", "registry"}:
            raise CanaryFailure("endpoint_not_configured")
        if entry.get("type") == "registry":
            command, argv, environment = load_registry_command(args, args.endpoint, entry)
            return command, argv, args.endpoint, environment
        command = entry.get("command")
        argv = entry.get("args")
        if not isinstance(command, str) or not isinstance(argv, list):
            raise CanaryFailure("invalid_endpoint_command")
        if not all(isinstance(item, str) for item in argv):
            raise CanaryFailure("invalid_endpoint_command")
        environment = entry.get("env", {})
        if not isinstance(environment, dict) or not all(
            isinstance(key, str) and isinstance(value, str)
            for key, value in environment.items()
        ):
            raise CanaryFailure("invalid_endpoint_environment")
        endpoint = args.endpoint
    if not Path(command).is_absolute():
        raise CanaryFailure("command_not_absolute")
    return command, argv, endpoint, environment


def classify_error(message: str) -> str:
    lowered = message.casefold()
    rules = (
        ("unsupported_route", r"method not found|unsupported|not supported|unknown model"),
        ("missing_executable", r"no such file|not found|exit (status: )?127"),
        ("authentication_expired", r"expired|token[^\n]*invalid"),
        (
            "authentication_required",
            r"auth|login|credential|unauthor|api.?key|access.?token",
        ),
        (
            "capacity_or_rate_limit",
            r"capacity|rate.?limit|quota|overloaded|spend|session limit",
        ),
        ("permission_denied", r"permission|forbidden|denied"),
        ("timeout", r"timed? ?out|timeout|deadline"),
    )
    for label, pattern in rules:
        if re.search(pattern, lowered):
            return label
    return "provider_or_transport_error"


def try_signal_process_group(process_group: int, signal_number: int) -> bool:
    """Signal only the owned process group without losing terminal evidence.

    A process group can disappear and its numeric id can be reused between the
    preceding liveness check and killpg.  On macOS that race can surface as
    EPERM when the reused group is no longer ours.  Treat the denial as an
    unproven cleanup result instead of crashing the canary or signalling a
    different group.
    """
    try:
        os.killpg(process_group, signal_number)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    return True


class Transport:
    def __init__(
        self, command: str, argv: list[str], cwd: Path, environment: dict[str, str]
    ) -> None:
        self.messages: queue.Queue[dict[str, Any] | None] = queue.Queue()
        self.stderr_sha256 = hashlib.sha256()
        self.stderr_bytes = 0
        self.stderr_classes: set[str] = set()
        self.cleanup_signal_denied = False
        try:
            self.process = subprocess.Popen(
                [command, *argv],
                cwd=cwd,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env={**os.environ, **environment},
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
            if not try_signal_process_group(self.process_group, signal.SIGTERM):
                self.cleanup_signal_denied = True
            deadline = time.monotonic() + grace_seconds
            while time.monotonic() < deadline and not self.group_is_gone():
                time.sleep(0.02)
        if not self.group_is_gone():
            if not try_signal_process_group(self.process_group, signal.SIGKILL):
                self.cleanup_signal_denied = True
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
            "cleanupSignalDenied": self.cleanup_signal_denied,
        }


class Journey:
    def __init__(
        self,
        transport: Transport,
        cwd: Path,
        sentinel: Path,
        sentinel_content: str,
        sentinel_sha256: str,
        timeout_seconds: float,
    ) -> None:
        self.transport = transport
        self.cwd = cwd
        self.sentinel = sentinel
        self.sentinel_content = sentinel_content
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
        self.close_session_supported = False
        self.close_session_completed = False
        self.stop_reason: str | None = None
        self.provider_error_code: int | str | None = None
        self.provider_error_message_sha256: str | None = None
        self.client_read_requests = 0
        self.client_read_completed = 0
        self.client_read_sentinel_matched = False

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

    def handle_read_text_file(self, message: dict[str, Any]) -> None:
        self.client_read_requests += 1
        request_id = message.get("id")
        params = message.get("params")
        if not isinstance(request_id, (int, str)) or not isinstance(params, dict):
            raise CanaryFailure("invalid_client_read_request")
        if params.get("sessionId") != self.session_id:
            raise CanaryFailure("invalid_client_read_request")
        raw_path = params.get("path")
        line = params.get("line")
        limit = params.get("limit")
        if (
            not isinstance(raw_path, str)
            or not Path(raw_path).is_absolute()
            or (line is not None and (not isinstance(line, int) or line < 1))
            or (limit is not None and (not isinstance(limit, int) or limit < 0))
        ):
            raise CanaryFailure("invalid_client_read_request")
        try:
            path = Path(raw_path).resolve(strict=True)
        except OSError as exc:
            raise CanaryFailure("invalid_client_read_request") from exc
        if not path.is_relative_to(self.cwd):
            raise CanaryFailure("client_read_outside_project")
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                metadata = os.fstat(fd)
                if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 8 * 1024 * 1024:
                    raise CanaryFailure("invalid_client_read_request")
                chunks: list[bytes] = []
                total = 0
                while True:
                    chunk = os.read(fd, 64 * 1024)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > 8 * 1024 * 1024:
                        raise CanaryFailure("invalid_client_read_request")
                    chunks.append(chunk)
            finally:
                os.close(fd)
            content = b"".join(chunks).decode("utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            raise CanaryFailure("invalid_client_read_request") from exc

        lines = content.splitlines(keepends=True)
        start = (line - 1) if line is not None else 0
        end = start + limit if limit is not None else None
        projected = "".join(lines[start:end])
        self.client_read_completed += 1
        if path == self.sentinel and sha256_text(content) == self.sentinel_sha256:
            self.client_read_sentinel_matched = True
        self.transport.send(
            {"jsonrpc": "2.0", "id": request_id, "result": {"content": projected}}
        )

    def await_response(
        self, request_id: int, deadline: float | None = None
    ) -> dict[str, Any]:
        response_deadline = self.deadline if deadline is None else deadline
        while True:
            message = self.transport.receive(response_deadline)
            self.observe(message)
            if message.get("method") == "session/request_permission":
                self.reject_permission(message)
            if message.get("method") == "fs/read_text_file":
                self.handle_read_text_file(message)
                continue
            if message.get("method") is not None and message.get("id") is not None:
                raise CanaryFailure("unsupported_client_request")
            if message.get("id") != request_id:
                continue
            if "error" in message:
                error = message.get("error")
                safe_message = error.get("message", "") if isinstance(error, dict) else ""
                if isinstance(error, dict) and isinstance(error.get("code"), (int, str)):
                    self.provider_error_code = error["code"]
                self.provider_error_message_sha256 = sha256_text(str(safe_message))
                raise CanaryFailure(classify_error(str(safe_message)))
            if "result" in message:
                return message

    def close_session(self, timeout_seconds: float) -> bool:
        if (
            not self.close_session_supported
            or self.session_id is None
            or self.close_session_completed
        ):
            return self.close_session_completed
        try:
            self.transport.send(
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/close",
                    "params": {"sessionId": self.session_id},
                }
            )
            self.await_response(3, time.monotonic() + max(timeout_seconds, 0.1))
        except (CanaryFailure, OSError, ValueError):
            return False
        self.close_session_completed = True
        return True

    def evidence(self, stop_reason: str | None = None) -> dict[str, Any]:
        completed = [
            fields
            for fields in self.tool_calls.values()
            if fields.get("status") == "completed"
        ]
        relative_sentinel = self.sentinel.relative_to(self.cwd)
        tool_input_matches = [
            tool_input_names_sentinel(
                fields.get("rawInput"), self.sentinel, relative_sentinel
            )
            for fields in completed
        ]
        tool_output_formats = [
            tool_output_sentinel_format(
                {
                    "content": fields.get("content"),
                    "rawOutput": fields.get("rawOutput"),
                },
                self.sentinel_content,
            )
            for fields in completed
        ]
        tool_evidence_formats = [
            output_format
            for input_matches, output_format in zip(
                tool_input_matches, tool_output_formats
            )
            if input_matches and output_format is not None
        ]
        tool_evidence_format = (
            tool_evidence_formats[0]
            if tool_evidence_formats
            else "client_read_exact"
            if self.client_read_sentinel_matched
            else None
        )
        return {
            "toolCallStarted": bool(self.tool_calls) or self.client_read_requests > 0,
            "toolCallCompleted": bool(completed) or self.client_read_completed > 0,
            "toolInputSentinelMatched": any(tool_input_matches),
            "toolOutputSentinelMatched": any(
                output_format is not None for output_format in tool_output_formats
            ),
            "toolEvidenceMatched": (
                bool(tool_evidence_formats) or self.client_read_sentinel_matched
            ),
            "toolEvidenceFormat": tool_evidence_format,
            "terminalMarkerObserved": self.marker in self.agent_text,
            "stopReason": stop_reason if stop_reason is not None else self.stop_reason,
            "providerErrorCode": self.provider_error_code,
            "providerErrorMessageSha256": self.provider_error_message_sha256,
            "clientReadRequestCount": self.client_read_requests,
            "clientReadCompletedCount": self.client_read_completed,
            "clientReadSentinelMatched": self.client_read_sentinel_matched,
            "agentMessageSha256": sha256_text(self.agent_text) if self.agent_text else None,
            "agentMessageClassification": (
                classify_error(self.agent_text) if self.agent_text else None
            ),
            "permissionRequestsObserved": self.permission_requests,
            "permissionRequestsApproved": 0,
            "closeSessionSupported": self.close_session_supported,
            "closeSessionCompleted": self.close_session_completed,
        }

    def run(self) -> dict[str, Any]:
        self.transport.send(INITIALIZE)
        initialized = self.await_response(0)
        initialize_result = initialized.get("result", {})
        if initialize_result.get("protocolVersion") != 1:
            raise CanaryFailure("unsupported_protocol")
        agent_capabilities = initialize_result.get("agentCapabilities")
        session_capabilities = (
            agent_capabilities.get("sessionCapabilities")
            if isinstance(agent_capabilities, dict)
            else None
        )
        self.close_session_supported = (
            isinstance(session_capabilities, dict)
            and session_capabilities.get("close") is not None
        )

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
            "Use a project filesystem tool to read "
            f"{json.dumps(str(relative_sentinel))} in the current project without modifying it. "
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
        self.stop_reason = stop_reason

        current_evidence = self.evidence(stop_reason)
        if not current_evidence["toolCallCompleted"]:
            message_class = current_evidence.get("agentMessageClassification")
            if message_class not in {None, "provider_or_transport_error"}:
                raise CanaryFailure(message_class)
            raise CanaryFailure("tool_evidence_missing")
        if not current_evidence["toolEvidenceMatched"]:
            raise CanaryFailure("project_evidence_mismatch")
        if not current_evidence["terminalMarkerObserved"]:
            raise CanaryFailure("terminal_marker_missing")

        if self.close_session_supported and not self.close_session(2.0):
            raise CanaryFailure("session_close_failed")
        return self.evidence(stop_reason)


def main() -> int:
    args = parse_args()
    started = time.monotonic()
    transport: Transport | None = None
    journey: Journey | None = None
    failure_class: str | None = None
    journey_evidence: dict[str, Any] = {}
    command = ""
    argv: list[str] = []
    environment: dict[str, str] = {}
    endpoint = args.endpoint or "direct"
    cwd: Path | None = None
    sentinel: Path | None = None
    sentinel_content = ""
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
        sentinel, sentinel_content, sentinel_sha256 = read_sentinel(cwd, args.sentinel)
        command, argv, endpoint, environment = load_command(args)
        transport = Transport(command, argv, cwd, environment)
        journey = Journey(
            transport,
            cwd,
            sentinel,
            sentinel_content,
            sentinel_sha256,
            args.timeout_seconds,
        )
        journey_evidence = journey.run()
    except CanaryFailure as exc:
        failure_class = exc.failure_class
    except (OSError, ValueError) as exc:
        failure_class = classify_error(str(exc))
    finally:
        if journey is not None and failure_class is not None:
            # A non-green project oracle must not strand the durable session it
            # created. Preserve the original failure classification even when
            # best-effort close is unsupported or fails.
            journey.close_session(min(args.termination_grace_seconds, 2.0))
            journey_evidence = journey.evidence()
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
        "environmentKeyNamesSha256": (
            sha256_bytes(b"\0".join(key.encode() for key in sorted(environment)))
            if environment
            else None
        ),
        "processStarted": transport is not None,
        "processGroupGone": process_group_gone,
        "toolCallStarted": journey_evidence.get("toolCallStarted", bool(journey and journey.tool_calls)),
        "toolCallCompleted": journey_evidence.get("toolCallCompleted", False),
        "toolInputSentinelMatched": journey_evidence.get(
            "toolInputSentinelMatched", False
        ),
        "toolOutputSentinelMatched": journey_evidence.get(
            "toolOutputSentinelMatched", False
        ),
        "toolEvidenceMatched": journey_evidence.get("toolEvidenceMatched", False),
        "toolEvidenceFormat": journey_evidence.get("toolEvidenceFormat"),
        "terminalMarkerObserved": journey_evidence.get("terminalMarkerObserved", False),
        "permissionRequestsObserved": journey_evidence.get(
            "permissionRequestsObserved", journey.permission_requests if journey else 0
        ),
        "permissionRequestsApproved": journey_evidence.get("permissionRequestsApproved", 0),
        "closeSessionSupported": journey_evidence.get(
            "closeSessionSupported",
            journey.close_session_supported if journey else False,
        ),
        "closeSessionCompleted": journey_evidence.get(
            "closeSessionCompleted",
            journey.close_session_completed if journey else False,
        ),
        "stopReason": journey_evidence.get("stopReason"),
        "providerErrorCode": journey_evidence.get("providerErrorCode"),
        "providerErrorMessageSha256": journey_evidence.get(
            "providerErrorMessageSha256"
        ),
        "clientReadRequestCount": journey_evidence.get("clientReadRequestCount", 0),
        "clientReadCompletedCount": journey_evidence.get(
            "clientReadCompletedCount", 0
        ),
        "clientReadSentinelMatched": journey_evidence.get(
            "clientReadSentinelMatched", False
        ),
        "agentMessageSha256": journey_evidence.get("agentMessageSha256"),
        "agentMessageClassification": journey_evidence.get(
            "agentMessageClassification"
        ),
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

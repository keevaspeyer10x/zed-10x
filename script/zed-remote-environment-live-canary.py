#!/usr/bin/env python3
"""Exercise the installed remote environment protocol across a real SSH seam.

The canary keeps its unique environment value in memory and sends it only in a
framed stdin prelude. Receipts contain hashes and booleans, never the value or
ambient process environments.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import select
import shlex
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path, PurePosixPath


MAX_ENVIRONMENT_BYTES = 1024 * 1024
READY_MARKER = "zed_live_env_ready"
COMPLETE_MARKER = "zed_live_env_complete"
APPLICATION_INPUT = b"zed-live-application-input\n"

NONINTERACTIVE_CHILD = r"""
import hashlib,json,os,sys
payload=sys.stdin.buffer.read(len(b'zed-live-application-input\n'))
sentinel_path=os.environ['ZED_LIVE_SENTINEL_PATH']
receipt={
  'cwd':os.getcwd(),
  'environmentSha256':hashlib.sha256(os.environ['ZED_LIVE_SECRET'].encode()).hexdigest(),
  'sentinelSha256':hashlib.sha256(open(sentinel_path,'rb').read()).hexdigest(),
  'applicationInputSha256':hashlib.sha256(payload).hexdigest(),
}
print(json.dumps(receipt,sort_keys=True,separators=(',',':')),flush=True)
""".strip()

PTY_CHILD = r"""
import hashlib,json,os,sys
payload=sys.stdin.buffer.readline()
sentinel_path=os.environ['ZED_LIVE_SENTINEL_PATH']
receipt={
  'cwd':os.getcwd(),
  'environmentSha256':hashlib.sha256(os.environ['ZED_LIVE_SECRET'].encode()).hexdigest(),
  'sentinelSha256':hashlib.sha256(open(sentinel_path,'rb').read()).hexdigest(),
  'applicationInputSha256':hashlib.sha256(payload).hexdigest(),
  'stdinIsTty':os.isatty(0),
}
print(json.dumps(receipt,sort_keys=True,separators=(',',':')),flush=True)
""".strip()

TCP_CHILD = r"""
import hashlib,json,os,socket,sys
port=int(sys.argv[1])
server=socket.socket()
server.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
server.bind(('127.0.0.1',port))
server.listen(1)
print('zed_live_tcp_ready',flush=True)
connection,_=server.accept()
payload=connection.recv(4096)
connection.sendall(b'Content-Length: 2\r\n\r\n{}')
connection.close(); server.close()
receipt={
  'environmentSha256':hashlib.sha256(os.environ['ZED_LIVE_SECRET'].encode()).hexdigest(),
  'protocolInputSha256':hashlib.sha256(payload).hexdigest(),
}
print(json.dumps(receipt,sort_keys=True,separators=(',',':')),flush=True)
""".strip()


class CanaryFailure(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--remote-server", required=True)
    parser.add_argument("--project", required=True)
    parser.add_argument("--sentinel", required=True)
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--ssh", default="ssh")
    parser.add_argument("--timeout", type=float, default=45.0)
    parser.add_argument("--log", type=Path)
    return parser.parse_args()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def encode_environment(environment: dict[str, str]) -> bytes:
    for name, value in environment.items():
        if not name or "=" in name or "\0" in name or "\0" in value:
            raise CanaryFailure("invalid_environment")
    payload = json.dumps(
        dict(sorted(environment.items())),
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode()
    if len(payload) > MAX_ENVIRONMENT_BYTES:
        raise CanaryFailure("environment_too_large")
    return str(len(payload)).encode() + b":" + payload + b","


def remote_command(
    remote_server: str,
    project: str,
    mode: str,
    child_script: str,
    child_args: tuple[str, ...] = (),
) -> str:
    server = PurePosixPath(remote_server)
    if server.is_absolute():
        server_expression = shlex.quote(str(server))
    else:
        server_expression = '"$HOME"/' + shlex.quote(str(server))
    argv = ["/usr/bin/python3", "-c", child_script, *child_args]
    bootstrap = [server_expression, mode]
    if mode == "env-exec-pty":
        bootstrap.extend(
            [
                "--ready-marker",
                shlex.quote(READY_MARKER),
                "--complete-marker",
                shlex.quote(COMPLETE_MARKER),
            ]
        )
    bootstrap.append("--")
    bootstrap.extend(shlex.quote(part) for part in argv)
    return f"cd {shlex.quote(project)} && exec {' '.join(bootstrap)}"


def owned_process_argv_is_private(pid: int, secret: str) -> bool:
    result = subprocess.run(
        ["/bin/ps", "-p", str(pid), "-o", "command="],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    return secret.encode() not in result.stdout


def terminate_owned(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


def run_capture(
    argv: list[str],
    stdin: bytes,
    secret: str,
    timeout: float,
) -> tuple[bytes, bytes, bool]:
    process = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    private = owned_process_argv_is_private(process.pid, secret)
    try:
        stdout, stderr = process.communicate(stdin, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        terminate_owned(process)
        raise CanaryFailure("owned_process_timeout") from error
    if process.returncode != 0:
        raise CanaryFailure(f"owned_process_exit_{process.returncode}")
    return stdout, stderr, private


def parse_receipt_line(output: bytes) -> dict[str, object]:
    for raw_line in reversed(output.replace(b"\r", b"").splitlines()):
        try:
            value = json.loads(raw_line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if isinstance(value, dict):
            return value
    raise CanaryFailure("missing_child_receipt")


def read_pty_line(master: int, timeout: float) -> bytes:
    deadline = time.monotonic() + timeout
    result = bytearray()
    while time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], min(0.25, deadline - time.monotonic()))
        if not readable:
            continue
        try:
            value = os.read(master, 1)
        except OSError as error:
            raise CanaryFailure("pty_read_failed") from error
        if not value:
            break
        result.extend(value)
        if value == b"\n":
            return bytes(result).replace(b"\r", b"").rstrip(b"\n")
    raise CanaryFailure("pty_line_timeout")


def run_pty(
    argv: list[str],
    frame: bytes,
    secret: str,
    timeout: float,
) -> tuple[dict[str, object], bool, bool]:
    master, slave = os.openpty()
    process = subprocess.Popen(
        argv,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        start_new_session=True,
    )
    os.close(slave)
    private = owned_process_argv_is_private(process.pid, secret)
    captured = bytearray()
    try:
        ready = read_pty_line(master, timeout)
        captured.extend(ready)
        if ready.decode(errors="replace") != READY_MARKER:
            raise CanaryFailure("pty_readiness_mismatch")
        os.write(master, frame)
        complete = read_pty_line(master, timeout)
        captured.extend(complete)
        if complete.decode(errors="replace") != COMPLETE_MARKER:
            raise CanaryFailure("pty_completion_mismatch")
        os.write(master, APPLICATION_INPUT)
        receipt = None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = read_pty_line(master, max(0.1, deadline - time.monotonic()))
            captured.extend(line)
            try:
                candidate = json.loads(line)
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            if isinstance(candidate, dict):
                receipt = candidate
                break
        if receipt is None:
            raise CanaryFailure("missing_pty_receipt")
        process.wait(timeout=timeout)
        if process.returncode != 0:
            raise CanaryFailure(f"pty_exit_{process.returncode}")
        return receipt, private, secret.encode() not in captured
    finally:
        os.close(master)
        terminate_owned(process)


def allocate_local_port() -> int:
    with socket.socket() as candidate:
        candidate.bind(("127.0.0.1", 0))
        return candidate.getsockname()[1]


def allocate_remote_port(ssh: str, host: str, timeout: float) -> int:
    script = "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()"
    command = f"/usr/bin/python3 -c {shlex.quote(script)}"
    result = subprocess.run(
        [ssh, host, command],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    return int(result.stdout.strip())


def run_tcp(
    args: argparse.Namespace,
    frame: bytes,
    secret: str,
) -> tuple[dict[str, object], bool]:
    local_port = allocate_local_port()
    remote_port = allocate_remote_port(args.ssh, args.host, args.timeout)
    command = remote_command(
        args.remote_server,
        args.project,
        "env-exec",
        TCP_CHILD,
        (str(remote_port),),
    )
    argv = [
        args.ssh,
        "-o",
        "ExitOnForwardFailure=yes",
        "-L",
        f"{local_port}:127.0.0.1:{remote_port}",
        args.host,
        command,
    ]
    process = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    private = owned_process_argv_is_private(process.pid, secret)
    protocol_input = b"Content-Length: 2\r\n\r\n{}"
    try:
        assert process.stdin is not None
        process.stdin.write(frame)
        process.stdin.flush()
        assert process.stdout is not None
        ready = process.stdout.readline().strip()
        if ready != b"zed_live_tcp_ready":
            raise CanaryFailure("tcp_readiness_mismatch")
        with socket.create_connection(("127.0.0.1", local_port), timeout=args.timeout) as stream:
            stream.sendall(protocol_input)
            response = stream.recv(4096)
        if response != b"Content-Length: 2\r\n\r\n{}":
            raise CanaryFailure("tcp_protocol_response_mismatch")
        stdout, stderr = process.communicate(timeout=args.timeout)
        if process.returncode != 0:
            raise CanaryFailure(f"tcp_exit_{process.returncode}")
        receipt = parse_receipt_line(stdout)
        receipt["protocolResponseSha256"] = sha256_bytes(response)
        return receipt, private
    except subprocess.TimeoutExpired as error:
        raise CanaryFailure("tcp_timeout") from error
    finally:
        terminate_owned(process)


def verify_capabilities(args: argparse.Namespace) -> list[str]:
    command = f"exec {shlex.quote(args.remote_server)} capabilities"
    stdout, _, _ = run_capture(
        [args.ssh, args.host, command],
        b"",
        "capability-check-has-no-secret",
        args.timeout,
    )
    capabilities = sorted(line for line in stdout.decode().splitlines() if line)
    if capabilities != ["env-exec-pty-v1", "env-exec-v1"]:
        raise CanaryFailure("capability_mismatch")
    return capabilities


def write_receipt(path: Path, receipt: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary_fd, temporary_name = tempfile.mkstemp(prefix=".zed-live-", dir=path.parent)
    try:
        os.fchmod(temporary_fd, 0o600)
        with os.fdopen(temporary_fd, "w") as output:
            json.dump(receipt, output, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_name, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass


def main() -> int:
    args = parse_args()
    secret = "zed live secret with spaces ' quotes \" unicode-λ and\nnewlines"
    environment = {
        "ZED_LIVE_SECRET": secret,
        "ZED_LIVE_SENTINEL_PATH": args.sentinel,
    }
    frame = encode_environment(environment)
    expected = {
        "environmentSha256": sha256_bytes(secret.encode()),
        "sentinelSha256": None,
        "applicationInputSha256": sha256_bytes(APPLICATION_INPUT),
        "protocolInputSha256": sha256_bytes(b"Content-Length: 2\r\n\r\n{}"),
    }
    receipt: dict[str, object] = {
        "schemaVersion": 1,
        "status": "failed",
        "host": args.host,
        "project": args.project,
        "remoteServerSha256": None,
        "capabilities": [],
        "secretPersisted": False,
    }
    try:
        capabilities = verify_capabilities(args)
        hash_stdout, _, _ = run_capture(
            [args.ssh, args.host, "sha256sum", args.remote_server],
            b"",
            secret,
            args.timeout,
        )
        receipt["remoteServerSha256"] = hash_stdout.decode().split()[0]
        sentinel_stdout, _, _ = run_capture(
            [args.ssh, args.host, "sha256sum", args.sentinel],
            b"",
            secret,
            args.timeout,
        )
        expected["sentinelSha256"] = sentinel_stdout.decode().split()[0]

        noninteractive_command = remote_command(
            args.remote_server,
            args.project,
            "env-exec",
            NONINTERACTIVE_CHILD,
        )
        noninteractive_stdout, noninteractive_stderr, noninteractive_private = run_capture(
            [args.ssh, args.host, noninteractive_command],
            frame + APPLICATION_INPUT,
            secret,
            args.timeout,
        )
        noninteractive = parse_receipt_line(noninteractive_stdout)
        if noninteractive_stderr:
            raise CanaryFailure("unexpected_noninteractive_stderr")

        pty_command = remote_command(
            args.remote_server,
            args.project,
            "env-exec-pty",
            PTY_CHILD,
        )
        pty, pty_private, pty_output_private = run_pty(
            [args.ssh, "-tt", args.host, pty_command],
            frame,
            secret,
            args.timeout,
        )
        tcp, tcp_private = run_tcp(args, frame, secret)

        for observed in (noninteractive, pty):
            for key in ("environmentSha256", "sentinelSha256", "applicationInputSha256"):
                if observed.get(key) != expected[key]:
                    raise CanaryFailure(f"{key}_mismatch")
            if observed.get("cwd") != args.project:
                raise CanaryFailure("cwd_mismatch")
        if pty.get("stdinIsTty") is not True:
            raise CanaryFailure("pty_not_allocated")
        for key in ("environmentSha256", "protocolInputSha256"):
            if tcp.get(key) != expected[key]:
                raise CanaryFailure(f"tcp_{key}_mismatch")

        log_private = True
        if args.log is not None and args.log.exists():
            log_private = secret.encode() not in args.log.read_bytes()
        argv_private = all((noninteractive_private, pty_private, tcp_private))
        if not argv_private or not pty_output_private or not log_private:
            raise CanaryFailure("secret_exposure")

        receipt.update(
            {
                "status": "passed",
                "capabilities": capabilities,
                "noninteractive": noninteractive,
                "pty": pty,
                "tcp": tcp,
                "localArgvPrivate": argv_private,
                "ptyOutputPrivate": pty_output_private,
                "logPrivate": log_private,
                "ownedProcessResidue": False,
            }
        )
    except Exception as error:  # noqa: BLE001 - terminal receipt is required for all canary failures.
        receipt["failureClass"] = type(error).__name__
        receipt["failureCode"] = str(error) if isinstance(error, CanaryFailure) else "unexpected"
    write_receipt(args.receipt, receipt)
    print(json.dumps({"status": receipt["status"], "receipt": str(args.receipt)}))
    return 0 if receipt["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())

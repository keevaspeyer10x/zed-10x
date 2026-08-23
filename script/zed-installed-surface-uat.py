#!/usr/bin/env python3
"""Prepare and verify the installed Zed remote-surface UAT fixture."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shlex
import shutil
import subprocess
import tarfile
import tempfile
from typing import Any


FIXTURE_ROOT = pathlib.Path(__file__).parent / "tests/fixtures/zed-installed-surface-uat"
PROJECT_TOKEN = "__ZED_UAT_PROJECT__"
REMOTE_PROJECT_PREFIX = "/home/keeva/uat/zed-"
TERMINAL_INPUT = b"zed installed terminal input\n"
VIM_INPUT = b"zed vim selected input\n"
SURFACE_VALUES = {
    "directory": "zed-directory-environment-v1",
    "terminal": "zed-terminal-setting-v1",
    "task": "zed-task-setting-v1",
    "mcp": "zed-mcp-setting-v1",
    "dap-stdio": "zed-dap-stdio-setting-v1",
    "dap-tcp": "zed-dap-tcp-setting-v1",
}
RECEIPT_FILES = {
    "terminal": "terminal.json",
    "task": "task.json",
    "vim": "vim.json",
    "mcp": "mcp.json",
    "dapStdio": "dap-stdio.json",
    "dapTcp": "dap-tcp.json",
}


class UatFailure(RuntimeError):
    pass


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    return sha256_bytes(path.read_bytes())


def validate_remote_project(value: str) -> str:
    path = pathlib.PurePosixPath(value)
    if not path.is_absolute() or not str(path).startswith(REMOTE_PROJECT_PREFIX):
        raise UatFailure("remote_project_outside_uat_root")
    if any(part in ("", ".", "..") for part in path.parts[1:]):
        raise UatFailure("remote_project_not_normalized")
    return str(path)


def write_receipt(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary_name = tempfile.mkstemp(prefix=".zed-uat-", dir=path.parent)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(value, output, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_name, path)
        directory_descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_descriptor)
        finally:
            os.close(directory_descriptor)
    finally:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass


def render_fixture(destination: pathlib.Path, remote_project: str) -> dict[str, str]:
    remote_project = validate_remote_project(remote_project)
    if destination.exists():
        raise UatFailure("render_destination_exists")
    shutil.copytree(
        FIXTURE_ROOT,
        destination,
        symlinks=False,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    for path in (destination / ".zed").glob("*.json"):
        rendered = path.read_text(encoding="utf-8").replace(PROJECT_TOKEN, remote_project)
        if PROJECT_TOKEN in rendered:
            raise UatFailure("project_token_remains")
        json.loads(rendered)
        path.write_text(rendered, encoding="utf-8")
    manifest = {
        str(path.relative_to(destination)): sha256_file(path)
        for path in sorted(destination.rglob("*"))
        if path.is_file()
    }
    if not manifest or any(path.is_symlink() for path in destination.rglob("*")):
        raise UatFailure("fixture_manifest_invalid")
    return manifest


def ssh_run(host: str, command: str, *, stdin: bytes | None = None) -> bytes:
    result = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", host, command],
        input=stdin,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=45,
        check=False,
    )
    if result.returncode != 0:
        raise UatFailure(f"ssh_exit_{result.returncode}")
    return result.stdout


def prepare(host: str, remote_project: str, receipt_path: pathlib.Path) -> dict[str, Any]:
    remote_project = validate_remote_project(remote_project)
    with tempfile.TemporaryDirectory(prefix="zed-installed-uat-") as directory:
        rendered = pathlib.Path(directory) / "fixture"
        manifest = render_fixture(rendered, remote_project)
        archive_path = pathlib.Path(directory) / "fixture.tar"
        with tarfile.open(archive_path, "w") as archive:
            for relative_path in sorted(manifest):
                archive.add(rendered / relative_path, arcname=relative_path, recursive=False)
        command = (
            "set -eu; "
            f"test ! -e {shlex.quote(remote_project)}; "
            f"install -d -m 700 {shlex.quote(remote_project)}; "
            f"tar -xf - -C {shlex.quote(remote_project)}; "
            f"/usr/bin/direnv allow {shlex.quote(remote_project)}"
        )
        ssh_run(host, command, stdin=archive_path.read_bytes())

    receipt = {
        "schemaVersion": 1,
        "status": "prepared",
        "host": host,
        "remoteProject": remote_project,
        "directoryEnvironmentAuthorizedBeforeOpen": True,
        "fixtureFiles": manifest,
        "journeyActions": [
            "open the exact remoteProject in the installed Zed 10x app",
            "run python3 terminal_uat.py, enter 'zed installed terminal input', resize, then send Ctrl-C",
            "run and cancel the 'ZED UAT Cancellable Task' task",
            "filter vim_input.txt through python3 vim_filter.py in Vim command mode",
            "start the zed-installed-uat context server and request tools/list",
            "run both ZED UAT DAP stdio and ZED UAT DAP TCP debug configurations",
        ],
    }
    write_receipt(receipt_path, receipt)
    return receipt


def collect_remote(host: str, remote_project: str) -> dict[str, Any]:
    remote_project = validate_remote_project(remote_project)
    script = """
import json,os,pathlib,sys
root=pathlib.Path(sys.argv[1]); observed={}; uat=root/'.uat'
for key,name in json.loads(sys.argv[2]).items():
 path=uat/name
 observed[key]={'bytes':path.read_text(encoding='utf-8'),'mode':path.stat().st_mode & 0o777}
residue=[]
for proc in pathlib.Path('/proc').iterdir():
 if not proc.name.isdigit(): continue
 try: argv=(proc/'cmdline').read_bytes().replace(b'\\0',b' ').decode(errors='replace')
 except (FileNotFoundError,PermissionError,ProcessLookupError): continue
 if str(root) in argv and any(name in argv for name in ('terminal_uat.py','task_uat.py','vim_filter.py','mcp_server.py','fake_dap.py')):
  residue.append(int(proc.name))
print(json.dumps({'receipts':observed,'processResidue':sorted(residue)},sort_keys=True))
""".strip()
    output = ssh_run(
        host,
        " ".join(
            [
                "/usr/bin/python3",
                "-c",
                shlex.quote(script),
                shlex.quote(remote_project),
                shlex.quote(json.dumps(RECEIPT_FILES, separators=(",", ":"))),
            ]
        ),
    )
    value = json.loads(output)
    if not isinstance(value, dict):
        raise UatFailure("remote_observation_invalid")
    return value


def expected_sha(name: str) -> str:
    return sha256_bytes(SURFACE_VALUES[name].encode())


def validate_observation(remote_project: str, observation: dict[str, Any]) -> dict[str, Any]:
    remote_project = validate_remote_project(remote_project)
    raw_receipts = observation.get("receipts")
    if not isinstance(raw_receipts, dict) or set(raw_receipts) != set(RECEIPT_FILES):
        raise UatFailure("receipt_set_mismatch")
    receipts: dict[str, dict[str, Any]] = {}
    receipt_hashes: dict[str, str] = {}
    for key, raw in raw_receipts.items():
        if not isinstance(raw, dict) or raw.get("mode") not in (0o600, 0o644):
            raise UatFailure(f"{key}_receipt_identity_invalid")
        encoded = raw.get("bytes")
        if not isinstance(encoded, str):
            raise UatFailure(f"{key}_receipt_missing")
        receipts[key] = json.loads(encoded)
        receipt_hashes[key] = sha256_bytes(encoded.encode())

    terminal = receipts["terminal"]
    if not (
        terminal.get("cwd") == remote_project
        and terminal.get("directoryEnvironmentSha256") == expected_sha("directory")
        and terminal.get("terminalEnvironmentSha256") == expected_sha("terminal")
        and terminal.get("inputSha256") == sha256_bytes(TERMINAL_INPUT)
        and terminal.get("stdinIsTty") is True
        and terminal.get("resizeCount", 0) >= 1
        and isinstance(terminal.get("observedColumns"), list)
        and len(set(terminal["observedColumns"])) >= 2
        and terminal.get("interrupted") is True
    ):
        raise UatFailure("terminal_journey_mismatch")

    task = receipts["task"]
    if not (
        task.get("cwd") == remote_project
        and task.get("directoryEnvironmentSha256") == expected_sha("directory")
        and task.get("taskEnvironmentSha256") == expected_sha("task")
        and task.get("stdinIsTty") is True
        and task.get("interrupted") is True
    ):
        raise UatFailure("task_journey_mismatch")

    vim = receipts["vim"]
    if not (
        vim.get("cwd") == remote_project
        and vim.get("directoryEnvironmentSha256") == expected_sha("directory")
        and vim.get("terminalEnvironmentSha256") == expected_sha("terminal")
        and vim.get("inputSha256") == sha256_bytes(VIM_INPUT)
    ):
        raise UatFailure("vim_journey_mismatch")

    mcp = receipts["mcp"]
    if not (
        mcp.get("cwd") == remote_project
        and mcp.get("environmentSha256") == expected_sha("mcp")
        and mcp.get("events") == ["initialize", "notifications/initialized", "tools/list"]
    ):
        raise UatFailure("mcp_journey_mismatch")

    expected_dap = {
        "dapStdio": ("dap-stdio", ["initialize", "launch", "configurationDone", "terminate"]),
        "dapTcp": (
            "dap-tcp",
            ["initialize", "launch", "configurationDone", "threads", "terminate"],
        ),
    }
    for key, (environment_name, events) in expected_dap.items():
        dap = receipts[key]
        matches = (
            dap.get("cwd") == remote_project
            and dap.get("environmentSha256") == expected_sha(environment_name)
            and dap.get("events") == events
        )
        if key == "dapTcp":
            matches = (
                matches
                and dap.get("tcpConnectionCount") == 2
                and dap.get("resetInitializeCount") == 1
                and dap.get("resetDelayMs", 0) > 100
            )
        if not matches:
            raise UatFailure(f"{key}_journey_mismatch")

    residue = observation.get("processResidue")
    if residue != []:
        raise UatFailure("fixture_process_residue")
    return {"status": "passed", "receiptSha256": receipt_hashes, "processResidue": []}


def verify(
    host: str,
    remote_project: str,
    receipt_path: pathlib.Path,
    log_path: pathlib.Path | None,
) -> dict[str, Any]:
    result = validate_observation(remote_project, collect_remote(host, remote_project))
    privacy_matches: list[str] = []
    if log_path is not None:
        log = log_path.read_text(encoding="utf-8", errors="replace")
        privacy_matches = [value for value in SURFACE_VALUES.values() if value in log]
        if "__aisw_prompt_check: command not found" in log:
            privacy_matches.append("orphaned_prompt_hook_warning")
    if privacy_matches:
        raise UatFailure("privacy_or_prompt_warning_match")
    receipt = {
        "schemaVersion": 1,
        "status": "passed",
        "host": host,
        "remoteProject": validate_remote_project(remote_project),
        "journeys": result,
        "logPrivacyMatches": [],
    }
    write_receipt(receipt_path, receipt)
    return receipt


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("prepare", "verify"):
        subparser = subparsers.add_parser(command)
        subparser.add_argument("--host", required=True)
        subparser.add_argument("--remote-project", required=True)
        subparser.add_argument("--receipt", required=True, type=pathlib.Path)
        if command == "verify":
            subparser.add_argument("--log", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "prepare":
            result = prepare(args.host, args.remote_project, args.receipt)
        else:
            result = verify(args.host, args.remote_project, args.receipt, args.log)
    except Exception as error:  # noqa: BLE001 - CLI needs one bounded failure projection.
        print(json.dumps({"status": "failed", "failure": type(error).__name__}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

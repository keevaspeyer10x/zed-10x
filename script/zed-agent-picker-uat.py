#!/usr/bin/env python3
"""Exercise every advertised Zed external-agent picker entry exactly once."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


SCHEMA = "zed-agent-picker-uat-v1"
INVENTORY_SCHEMA = "zed-agent-picker-inventory-v1"
EXTERNAL_FAILURES = {
    "authentication_expired",
    "authentication_required",
    "capacity_or_rate_limit",
}
INTERACTION_FAILURES = {"permission_requested"}


class MatrixFailure(RuntimeError):
    def __init__(self, failure_class: str) -> None:
        super().__init__(failure_class)
        self.failure_class = failure_class


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--surface", required=True)
    parser.add_argument("--inventory", required=True, type=Path)
    parser.add_argument("--source-manifest", required=True, type=Path)
    parser.add_argument("--settings", required=True, type=Path)
    parser.add_argument("--registry-cache", required=True, type=Path)
    parser.add_argument("--npm-command", required=True, type=Path)
    parser.add_argument("--cwd", required=True, type=Path)
    parser.add_argument("--sentinel", required=True, type=Path)
    parser.add_argument("--ephemeral-sentinel", action="store_true")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--summary", required=True, type=Path)
    parser.add_argument(
        "--canary",
        type=Path,
        default=Path(__file__).with_name("zed-acp-live-canary.py"),
    )
    parser.add_argument("--timeout-seconds", type=float, default=180)
    parser.add_argument("--termination-grace-seconds", type=float, default=5)
    return parser.parse_args()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(64 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def load_canary_module(path: Path) -> Any:
    spec = importlib.util.spec_from_file_location("zed_acp_live_canary", path)
    if spec is None or spec.loader is None:
        raise MatrixFailure("invalid_canary")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_json_file(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise MatrixFailure("invalid_inventory") from exc


def load_settings(path: Path, canary: Any) -> dict[str, Any]:
    try:
        value = json.loads(canary.strip_jsonc(path.read_text(encoding="utf-8")))
    except (OSError, UnicodeError, json.JSONDecodeError, canary.CanaryFailure) as exc:
        raise MatrixFailure("invalid_settings") from exc
    if not isinstance(value, dict) or not isinstance(value.get("agent_servers"), dict):
        raise MatrixFailure("invalid_settings")
    return value


def validate_private_directory(path: Path) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise MatrixFailure("invalid_output_directory") from exc
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise MatrixFailure("invalid_output_directory")


def load_inventory(
    path: Path, surface: str
) -> tuple[dict[str, Any], list[str], set[str]]:
    inventory = load_json_file(path)
    if not isinstance(inventory, dict) or inventory.get("schema") != INVENTORY_SCHEMA:
        raise MatrixFailure("invalid_inventory")
    surfaces = inventory.get("surfaces")
    managed = inventory.get("managedEntries")
    execution_classes = inventory.get("executionClasses")
    expected = surfaces.get(surface) if isinstance(surfaces, dict) else None
    if (
        not isinstance(expected, list)
        or not expected
        or not all(isinstance(item, str) and item for item in expected)
        or len(set(expected)) != len(expected)
        or not isinstance(managed, list)
        or not all(isinstance(item, str) and item for item in managed)
        or len(set(managed)) != len(managed)
        or not set(expected).issubset(set(managed))
        or not isinstance(execution_classes, dict)
        or set(execution_classes)
        != {
            "mac-custom",
            "mac-registry",
            "intrepid-local",
            "intrepid-persistent",
            "intrepid-registry",
        }
    ):
        raise MatrixFailure("invalid_inventory")
    for class_id, execution_class in execution_classes.items():
        if not isinstance(execution_class, dict):
            raise MatrixFailure("invalid_inventory")
        class_surface = execution_class.get("surface")
        representative = execution_class.get("representative")
        members = execution_class.get("members")
        if (
            class_surface not in surfaces
            or not isinstance(members, list)
            or not all(isinstance(item, str) and item for item in members)
            or len(set(members)) != len(members)
            or (members and representative not in members)
            or (not members and representative is not None)
            or not set(members).issubset(set(surfaces[class_surface]))
        ):
            raise MatrixFailure("invalid_inventory")
    for surface_id, surface_entries in surfaces.items():
        class_members = [
            member
            for execution_class in execution_classes.values()
            if execution_class["surface"] == surface_id
            for member in execution_class["members"]
        ]
        if class_members != surface_entries or len(set(class_members)) != len(class_members):
            raise MatrixFailure("invalid_inventory")
    return inventory, expected, set(managed)


def validate_source_manifest(
    path: Path, inventory: dict[str, Any]
) -> None:
    manifest = load_json_file(path)
    if not isinstance(manifest, dict) or manifest.get("schemaVersion") != 2:
        raise MatrixFailure("invalid_source_manifest")
    managed = manifest.get("managedNames")
    mac = manifest.get("macLanes")
    linux_local = manifest.get("linuxLocalLanes")
    persistent = manifest.get("persistentLanes")
    registry = manifest.get("projectHostRegistryLanes")
    registry_exclusions = manifest.get("projectHostRegistryExclusions")
    servers = manifest.get("agentServers")
    if not (
        isinstance(managed, list)
        and all(isinstance(item, str) and item for item in managed)
        and len(managed) == len(set(managed))
        and isinstance(mac, list)
        and len(mac) == len(set(mac))
        and isinstance(linux_local, list)
        and len(linux_local) == len(set(linux_local))
        and isinstance(persistent, dict)
        and isinstance(registry, list)
        and len(registry) == len(set(registry))
        and isinstance(servers, dict)
        and all(isinstance(item, str) and item for item in mac + linux_local + registry)
        and all(isinstance(item, str) and item for item in persistent)
        and isinstance(registry_exclusions, dict)
        and set(registry_exclusions) == {"Darwin", "Linux"}
        and all(
            isinstance(exclusions, list)
            and all(isinstance(item, str) and item for item in exclusions)
            and len(exclusions) == len(set(exclusions))
            and set(exclusions).issubset(registry)
            for exclusions in registry_exclusions.values()
        )
        and set(registry_exclusions["Darwin"]).isdisjoint(
            registry_exclusions["Linux"]
        )
        and set(managed)
        == set(mac) | set(linux_local) | set(persistent) | set(registry)
        and set(servers) == set(managed)
        and set(mac).isdisjoint(linux_local)
        and set(mac).isdisjoint(persistent)
        and set(mac).isdisjoint(registry)
        and set(linux_local).isdisjoint(persistent)
        and set(linux_local).isdisjoint(registry)
        and set(persistent).isdisjoint(registry)
    ):
        raise MatrixFailure("invalid_source_manifest")
    mac_registry = [
        item for item in registry if item not in registry_exclusions["Darwin"]
    ]
    linux_registry = [
        item for item in registry if item not in registry_exclusions["Linux"]
    ]
    projected = {
        "managedEntries": managed,
        "surfaces": {
            "mac-local": mac + mac_registry,
            "intrepid": linux_local + list(persistent) + linux_registry,
        },
        "executionClassMembers": {
            "mac-custom": mac,
            "mac-registry": mac_registry,
            "intrepid-local": linux_local,
            "intrepid-persistent": list(persistent),
            "intrepid-registry": linux_registry,
        },
    }
    if (
        projected["managedEntries"] != inventory.get("managedEntries")
        or projected["surfaces"] != inventory.get("surfaces")
        or set(servers) != set(managed)
        or projected["executionClassMembers"]
        != {
            class_id: execution_class.get("members")
            for class_id, execution_class in inventory.get("executionClasses", {}).items()
        }
    ):
        raise MatrixFailure("source_inventory_mismatch")


def safe_receipt_name(index: int, endpoint: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", endpoint).strip("-") or "endpoint"
    return f"{index:02d}-{slug}.json"


def main() -> int:
    args = parse_args()
    started = time.monotonic()
    failure_class: str | None = None
    expected: list[str] = []
    configured_managed: list[str] = []
    results: list[dict[str, Any]] = []

    try:
        if args.summary.exists():
            raise MatrixFailure("summary_already_exists")
        validate_private_directory(args.output_dir)
        for path, failure in (
            (args.inventory, "invalid_inventory"),
            (args.source_manifest, "invalid_source_manifest"),
            (args.settings, "invalid_settings"),
            (args.registry_cache, "invalid_registry_cache"),
            (args.npm_command, "invalid_npm_command"),
            (args.canary, "invalid_canary"),
        ):
            if not path.is_absolute() or not path.exists():
                raise MatrixFailure(failure)
        if not args.npm_command.is_file() or not os.access(args.npm_command, os.X_OK):
            raise MatrixFailure("invalid_npm_command")
        if not 0.5 <= args.timeout_seconds <= 900:
            raise MatrixFailure("invalid_timeout")

        canary = load_canary_module(Path(__file__).with_name("zed-acp-live-canary.py"))
        if not args.cwd.is_absolute():
            raise MatrixFailure("invalid_project_directory")
        try:
            project_cwd = args.cwd.resolve(strict=True)
            if not project_cwd.is_dir():
                raise MatrixFailure("invalid_project_directory")
        except OSError as exc:
            raise MatrixFailure("invalid_project_directory") from exc
        if args.ephemeral_sentinel:
            if (
                args.sentinel.is_absolute()
                or args.sentinel.parent != Path(".")
                or args.sentinel.name in {"", ".", ".."}
                or (project_cwd / args.sentinel).exists()
            ):
                raise MatrixFailure("invalid_sentinel")
        else:
            try:
                canary.read_sentinel(project_cwd, args.sentinel)
            except (OSError, ValueError, canary.CanaryFailure) as exc:
                raise MatrixFailure("invalid_sentinel") from exc
        inventory, expected, managed = load_inventory(args.inventory, args.surface)
        validate_source_manifest(args.source_manifest, inventory)
        settings = load_settings(args.settings, canary)
        configured = set(settings["agent_servers"])
        configured_managed = sorted(configured & managed)
        if configured & managed != set(expected):
            raise MatrixFailure("picker_inventory_mismatch")

        for index, endpoint in enumerate(expected, start=1):
            receipt_path = args.output_dir / safe_receipt_name(index, endpoint)
            command = [
                sys.executable,
                str(args.canary),
                "--surface",
                args.surface,
                "--cwd",
                str(args.cwd),
                "--sentinel",
                str(args.sentinel),
                *(["--ephemeral-sentinel"] if args.ephemeral_sentinel else []),
                "--output",
                str(receipt_path),
                "--timeout-seconds",
                str(args.timeout_seconds),
                "--termination-grace-seconds",
                str(args.termination_grace_seconds),
                "--settings",
                str(args.settings),
                "--endpoint",
                endpoint,
                "--registry-cache",
                str(args.registry_cache),
                "--npm-command",
                str(args.npm_command),
            ]
            process = subprocess.run(
                command,
                cwd=args.cwd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=args.timeout_seconds + args.termination_grace_seconds + 15,
                check=False,
            )
            if not receipt_path.is_file():
                raise MatrixFailure("missing_endpoint_receipt")
            receipt = load_json_file(receipt_path)
            if (
                not isinstance(receipt, dict)
                or receipt.get("schema") != canary.SCHEMA
                or receipt.get("endpoint") != endpoint
                or receipt.get("processGroupGone") is not True
                or receipt.get("promptOrResponseContentRetained") is not False
                or (
                    args.ephemeral_sentinel
                    and (
                        receipt.get("ephemeralSentinel") is not True
                        or receipt.get("sentinelCreated") is not True
                        or receipt.get("sentinelRemoved") is not True
                        or (project_cwd / args.sentinel).exists()
                    )
                )
            ):
                raise MatrixFailure("invalid_endpoint_receipt")
            endpoint_failure = receipt.get("failureClass")
            status = receipt.get("status")
            if process.returncode == 0 and status == "pass" and endpoint_failure is None:
                classification = "passed"
            elif (
                process.returncode != 0
                and status == "failed"
                and endpoint_failure in EXTERNAL_FAILURES
            ):
                classification = "external_unavailable"
            elif (
                process.returncode != 0
                and status == "failed"
                and endpoint_failure in INTERACTION_FAILURES
                and receipt.get("permissionRequestsObserved") == 1
                and receipt.get("permissionRequestsApproved") == 0
            ):
                classification = "interaction_required"
            else:
                classification = "product_failure"
            results.append(
                {
                    "endpoint": endpoint,
                    "classification": classification,
                    "failureClass": endpoint_failure,
                    "receiptSha256": sha256_file(receipt_path),
                    "elapsedMs": receipt.get("elapsedMs"),
                }
            )

        if [result["endpoint"] for result in results] != expected:
            raise MatrixFailure("incomplete_picker_coverage")
        if any(result["classification"] == "product_failure" for result in results):
            raise MatrixFailure("picker_product_failure")
    except subprocess.TimeoutExpired:
        failure_class = "matrix_subprocess_timeout"
    except MatrixFailure as exc:
        failure_class = exc.failure_class
    except (OSError, ValueError) as exc:
        failure_class = "matrix_runtime_error"

    summary = {
        "schema": SCHEMA,
        "status": "pass" if failure_class is None else "failed",
        "surface": args.surface,
        "failureClass": failure_class,
        "expectedEndpoints": expected,
        "configuredManagedEndpoints": configured_managed,
        "inventorySha256": sha256_file(args.inventory) if args.inventory.is_file() else None,
        "sourceManifestSha256": (
            sha256_file(args.source_manifest) if args.source_manifest.is_file() else None
        ),
        "settingsSha256": sha256_file(args.settings) if args.settings.is_file() else None,
        "registrySha256": (
            sha256_file(args.registry_cache / "registry.json")
            if (args.registry_cache / "registry.json").is_file()
            else None
        ),
        "canarySha256": sha256_file(args.canary) if args.canary.is_file() else None,
        "cwdSha256": sha256_text(str(args.cwd.resolve(strict=False))),
        "ephemeralSentinel": args.ephemeral_sentinel,
        "results": results,
        "passedCount": sum(result["classification"] == "passed" for result in results),
        "externalUnavailableCount": sum(
            result["classification"] == "external_unavailable" for result in results
        ),
        "interactionRequiredCount": sum(
            result["classification"] == "interaction_required" for result in results
        ),
        "productFailureCount": sum(
            result["classification"] == "product_failure" for result in results
        ),
        "contentRetained": False,
        "elapsedMs": round((time.monotonic() - started) * 1000),
    }
    try:
        canary = load_canary_module(Path(__file__).with_name("zed-acp-live-canary.py"))
        canary.write_exclusive(args.summary, summary)
    except FileExistsError:
        print("picker UAT summary already exists", file=sys.stderr)
        return 2
    except (MatrixFailure, OSError):
        return 2
    print(json.dumps(summary, sort_keys=True))
    return 0 if failure_class is None else 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Validate exact picker receipts and publish one content-free route matrix."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
from pathlib import Path
from typing import Any


SUMMARY_SCHEMA = "zed-agent-picker-uat-v1"
RECEIPT_SCHEMA = "zed-acp-project-canary-v1"
OUTPUT_SCHEMA = "zed-agent-matrix-v2"
EXTERNAL_FAILURES = {
    "authentication_expired",
    "authentication_required",
    "capacity_or_rate_limit",
}
CLASSIFICATIONS = {"passed", "external_unavailable", "interaction_required"}


class AggregateFailure(RuntimeError):
    pass


def is_sha256(value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", required=True, type=Path)
    parser.add_argument("--source-manifest", required=True, type=Path)
    parser.add_argument("--tested-revision", required=True)
    parser.add_argument("--tested-tree", required=True)
    parser.add_argument("--attempt", required=True, type=int)
    parser.add_argument("--mac-summary", required=True, type=Path)
    parser.add_argument("--mac-receipts", required=True, type=Path)
    parser.add_argument("--intrepid-summary", required=True, type=Path)
    parser.add_argument("--intrepid-receipts", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def load_regular_json(path: Path) -> Any:
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
            raise AggregateFailure("non_regular_evidence")
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise AggregateFailure("invalid_evidence") from exc


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(64 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_receipt_name(index: int, endpoint: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", endpoint).strip("-") or "endpoint"
    return f"{index:02d}-{slug}.json"


def validate_surface(
    *,
    surface: str,
    expected: list[str],
    summary_path: Path,
    receipts_dir: Path,
    inventory_sha256: str,
    source_manifest_sha256: str,
    used_receipts: set[str],
) -> dict[str, Any]:
    summary = load_regular_json(summary_path)
    if (
        not isinstance(summary, dict)
        or summary.get("schema") != SUMMARY_SCHEMA
        or summary.get("status") != "pass"
        or summary.get("failureClass") is not None
        or summary.get("surface") != surface
        or summary.get("expectedEndpoints") != expected
        or set(summary.get("configuredManagedEndpoints", [])) != set(expected)
        or summary.get("inventorySha256") != inventory_sha256
        or summary.get("sourceManifestSha256") != source_manifest_sha256
        or not is_sha256(summary.get("settingsSha256"))
        or not is_sha256(summary.get("registrySha256"))
        or not is_sha256(summary.get("canarySha256"))
        or summary.get("contentRetained") is not False
        or not isinstance(summary.get("results"), list)
        or len(summary["results"]) != len(expected)
    ):
        raise AggregateFailure("invalid_surface_summary")

    routes: list[dict[str, Any]] = []
    counts = {classification: 0 for classification in CLASSIFICATIONS}
    # Exact lengths were established above, so ordinary zip is both strict in
    # practice and compatible with the oldest Python supported by the release
    # harness.
    for index, (endpoint, result) in enumerate(zip(expected, summary["results"]), start=1):
        if not isinstance(result, dict) or result.get("endpoint") != endpoint:
            raise AggregateFailure("route_order_mismatch")
        classification = result.get("classification")
        failure_class = result.get("failureClass")
        if classification not in CLASSIFICATIONS:
            raise AggregateFailure("invalid_route_classification")
        if (
            classification == "passed" and failure_class is not None
        ) or (
            classification == "external_unavailable"
            and failure_class not in EXTERNAL_FAILURES
        ) or (
            classification == "interaction_required"
            and failure_class != "permission_requested"
        ):
            raise AggregateFailure("classification_failure_mismatch")

        receipt_path = receipts_dir / safe_receipt_name(index, endpoint)
        receipt = load_regular_json(receipt_path)
        receipt_sha256 = sha256_file(receipt_path)
        if receipt_sha256 in used_receipts:
            raise AggregateFailure("reused_route_receipt")
        used_receipts.add(receipt_sha256)
        expected_status = "pass" if classification == "passed" else "failed"
        if (
            result.get("receiptSha256") != receipt_sha256
            or not isinstance(receipt, dict)
            or receipt.get("schema") != RECEIPT_SCHEMA
            or receipt.get("surface") != surface
            or receipt.get("endpoint") != endpoint
            or receipt.get("status") != expected_status
            or receipt.get("failureClass") != failure_class
            or receipt.get("processGroupGone") is not True
            or receipt.get("promptOrResponseContentRetained") is not False
            or receipt.get("permissionRequestsApproved") != 0
            or not isinstance(receipt.get("elapsedMs"), (int, float))
            or receipt["elapsedMs"] < 0
        ):
            raise AggregateFailure("receipt_result_mismatch")
        counts[classification] += 1
        routes.append(
            {
                "endpoint": endpoint,
                "classification": classification,
                "failureClass": failure_class,
                "receiptSha256": receipt_sha256,
                "elapsedMs": receipt.get("elapsedMs"),
            }
        )

    if (
        summary.get("passedCount") != counts["passed"]
        or summary.get("externalUnavailableCount") != counts["external_unavailable"]
        or summary.get("interactionRequiredCount") != counts["interaction_required"]
        or summary.get("productFailureCount") != 0
    ):
        raise AggregateFailure("summary_count_mismatch")
    return {
        "surface": surface,
        "summarySha256": sha256_file(summary_path),
        "settingsSha256": summary.get("settingsSha256"),
        "registrySha256": summary.get("registrySha256"),
        "canarySha256": summary.get("canarySha256"),
        "expectedCount": len(expected),
        "counts": counts,
        "routes": routes,
    }


def write_exclusive(path: Path, payload: dict[str, Any]) -> None:
    encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode()
    descriptor = os.open(
        path,
        os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as target:
            target.write(encoded)
            target.flush()
            os.fsync(target.fileno())
    finally:
        os.close(descriptor)
    directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def main() -> int:
    args = parse_args()
    if (
        not re.fullmatch(r"[0-9a-f]{40}", args.tested_revision)
        or not re.fullmatch(r"[0-9a-f]{40}", args.tested_tree)
        or args.attempt < 1
        or not args.output.is_absolute()
        or not args.output.parent.is_dir()
    ):
        raise AggregateFailure("invalid_invocation")
    inventory = load_regular_json(args.inventory)
    if (
        not isinstance(inventory, dict)
        or inventory.get("schema") != "zed-agent-picker-inventory-v1"
        or not isinstance(inventory.get("surfaces"), dict)
    ):
        raise AggregateFailure("invalid_inventory")
    surfaces = inventory["surfaces"]
    if set(surfaces) != {"mac-local", "intrepid"} or not all(
        isinstance(entries, list)
        and entries
        and all(isinstance(entry, str) and entry for entry in entries)
        and len(entries) == len(set(entries))
        for entries in surfaces.values()
    ):
        raise AggregateFailure("invalid_inventory")

    inventory_sha256 = sha256_file(args.inventory)
    source_manifest_sha256 = sha256_file(args.source_manifest)
    used_receipts: set[str] = set()
    result = {
        "schema": OUTPUT_SCHEMA,
        "status": "pass",
        "evidenceMode": "standalone_project_canary",
        "testedRevision": args.tested_revision,
        "testedTree": args.tested_tree,
        "attempt": args.attempt,
        "inventorySha256": inventory_sha256,
        "sourceManifestSha256": source_manifest_sha256,
        "surfaces": [
            validate_surface(
                surface="mac-local",
                expected=surfaces["mac-local"],
                summary_path=args.mac_summary,
                receipts_dir=args.mac_receipts,
                inventory_sha256=inventory_sha256,
                source_manifest_sha256=source_manifest_sha256,
                used_receipts=used_receipts,
            ),
            validate_surface(
                surface="intrepid",
                expected=surfaces["intrepid"],
                summary_path=args.intrepid_summary,
                receipts_dir=args.intrepid_receipts,
                inventory_sha256=inventory_sha256,
                source_manifest_sha256=source_manifest_sha256,
                used_receipts=used_receipts,
            ),
        ],
        "permissionsApproved": 0,
        "promptOrResponseContentRetained": False,
        "allDirectProcessGroupsGone": True,
    }
    write_exclusive(args.output, result)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AggregateFailure as exc:
        print(str(exc), file=os.sys.stderr)
        raise SystemExit(1) from exc

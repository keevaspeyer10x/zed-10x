#!/usr/bin/env python3
"""Deterministic receipt fixture for the picker-matrix orchestrator."""

import argparse
import json
import os
from pathlib import Path


parser = argparse.ArgumentParser()
parser.add_argument("--endpoint", required=True)
parser.add_argument("--output", required=True, type=Path)
args, _ = parser.parse_known_args()

failure = None
if "Auth" in args.endpoint:
    failure = "authentication_required"
elif "Permission" in args.endpoint:
    failure = "permission_requested"
elif "Product" in args.endpoint:
    failure = "missing_executable"
elif "Unsupported" in args.endpoint:
    failure = "unsupported_client_request"

receipt = {
    "schema": "zed-acp-project-canary-v1",
    "status": "failed" if failure else "pass",
    "endpoint": args.endpoint,
    "failureClass": failure,
    "processGroupGone": True,
    "promptOrResponseContentRetained": False,
    "permissionRequestsObserved": 1 if failure == "permission_requested" else 0,
    "permissionRequestsApproved": 0,
    "elapsedMs": 1,
}
args.output.write_text(json.dumps(receipt) + "\n", encoding="utf-8")
log = os.environ.get("FAKE_CANARY_LOG")
if log:
    with Path(log).open("a", encoding="utf-8") as target:
        target.write(args.endpoint + "\n")
raise SystemExit(1 if failure else 0)

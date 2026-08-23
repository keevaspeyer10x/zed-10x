#!/usr/bin/env python3
import hashlib
import json
import os
import signal


receipt_path = os.path.join(os.getcwd(), ".uat", "task.json")
state = {
    "cwd": os.getcwd(),
    "directoryEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_DIRECTORY_VALUE", "").encode()
    ).hexdigest(),
    "taskEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_TASK_VALUE", "").encode()
    ).hexdigest(),
    "stdinIsTty": os.isatty(0),
    "interrupted": False,
}


def persist() -> None:
    os.makedirs(os.path.dirname(receipt_path), mode=0o700, exist_ok=True)
    with open(receipt_path, "w", encoding="utf-8") as receipt:
        json.dump(state, receipt, sort_keys=True, separators=(",", ":"))
        receipt.write("\n")


def interrupted(_signum, _frame) -> None:
    state["interrupted"] = True
    persist()
    raise SystemExit(130)


signal.signal(signal.SIGINT, interrupted)
signal.signal(signal.SIGTERM, interrupted)
persist()
print("ZED_UAT_TASK_READY", flush=True)
signal.pause()

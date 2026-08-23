#!/usr/bin/env python3
import hashlib
import json
import os
import signal
import sys


receipt_path = os.path.join(os.getcwd(), ".uat", "terminal.json")
state = {
    "cwd": os.getcwd(),
    "directoryEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_DIRECTORY_VALUE", "").encode()
    ).hexdigest(),
    "terminalEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_TERMINAL_VALUE", "").encode()
    ).hexdigest(),
    "stdinIsTty": os.isatty(0),
    "initialColumns": os.get_terminal_size(0).columns,
    "observedColumns": [os.get_terminal_size(0).columns],
    "resizeCount": 0,
    "inputSha256": None,
    "interrupted": False,
}


def persist() -> None:
    os.makedirs(os.path.dirname(receipt_path), mode=0o700, exist_ok=True)
    with open(receipt_path, "w", encoding="utf-8") as receipt:
        json.dump(state, receipt, sort_keys=True, separators=(",", ":"))
        receipt.write("\n")


def resized(_signum, _frame) -> None:
    state["resizeCount"] += 1
    columns = os.get_terminal_size(0).columns
    state["finalColumns"] = columns
    if columns not in state["observedColumns"]:
        state["observedColumns"].append(columns)
    persist()


def interrupted(_signum, _frame) -> None:
    state["interrupted"] = True
    persist()
    raise SystemExit(130)


signal.signal(signal.SIGWINCH, resized)
signal.signal(signal.SIGINT, interrupted)
persist()
print("ZED_UAT_TERMINAL_READY", flush=True)
value = sys.stdin.readline().encode()
state["inputSha256"] = hashlib.sha256(value).hexdigest()
persist()
print("ZED_UAT_TERMINAL_WAITING_FOR_INTERRUPT", flush=True)
while True:
    signal.pause()

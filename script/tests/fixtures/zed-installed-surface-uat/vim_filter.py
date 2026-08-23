#!/usr/bin/env python3
import hashlib
import json
import os
import sys


payload = sys.stdin.buffer.read()
receipt = {
    "cwd": os.getcwd(),
    "directoryEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_DIRECTORY_VALUE", "").encode()
    ).hexdigest(),
    "terminalEnvironmentSha256": hashlib.sha256(
        os.environ.get("ZED_UAT_TERMINAL_VALUE", "").encode()
    ).hexdigest(),
    "inputSha256": hashlib.sha256(payload).hexdigest(),
}
os.makedirs(".uat", mode=0o700, exist_ok=True)
with open(".uat/vim.json", "w", encoding="utf-8") as output:
    json.dump(receipt, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
sys.stdout.buffer.write(payload.upper())

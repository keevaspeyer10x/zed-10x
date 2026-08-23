#!/usr/bin/env python3
import hashlib
import json
import os
import sys


events = []
receipt_path = os.path.join(os.getcwd(), ".uat", "mcp.json")


def persist() -> None:
    os.makedirs(os.path.dirname(receipt_path), mode=0o700, exist_ok=True)
    receipt = {
        "cwd": os.getcwd(),
        "environmentSha256": hashlib.sha256(
            os.environ.get("ZED_UAT_MCP_VALUE", "").encode()
        ).hexdigest(),
        "events": events,
    }
    with open(receipt_path, "w", encoding="utf-8") as output:
        json.dump(receipt, output, sort_keys=True, separators=(",", ":"))
        output.write("\n")


persist()
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method:
        events.append(method)
        persist()
    request_id = request.get("id")
    if request_id is None:
        continue
    if method == "initialize":
        result = {
            "protocolVersion": request.get("params", {}).get(
                "protocolVersion", "2025-03-26"
            ),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "zed-installed-uat", "version": "1"},
        }
    elif method == "tools/list":
        result = {"tools": []}
    elif method == "ping":
        result = {}
    else:
        result = {}
    print(
        json.dumps(
            {"jsonrpc": "2.0", "id": request_id, "result": result},
            separators=(",", ":"),
        ),
        flush=True,
    )

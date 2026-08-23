#!/usr/bin/env python3
import hashlib
import json
import os
import socket
import struct
import sys
import time


def write_receipt(
    mode: str,
    events: list[str],
    *,
    tcp_connection_count: int = 0,
    reset_initialize_count: int = 0,
    reset_delay_ms: int = 0,
) -> None:
    os.makedirs(".uat", mode=0o700, exist_ok=True)
    name = "dap-tcp.json" if mode == "tcp" else "dap-stdio.json"
    env_name = "ZED_UAT_DAP_TCP_VALUE" if mode == "tcp" else "ZED_UAT_DAP_STDIO_VALUE"
    receipt = {
        "cwd": os.getcwd(),
        "environmentSha256": hashlib.sha256(os.environ.get(env_name, "").encode()).hexdigest(),
        "events": events,
    }
    if mode == "tcp":
        receipt.update(
            {
                "tcpConnectionCount": tcp_connection_count,
                "resetInitializeCount": reset_initialize_count,
                "resetDelayMs": reset_delay_ms,
            }
        )
    with open(os.path.join(".uat", name), "w", encoding="utf-8") as output:
        json.dump(receipt, output, sort_keys=True, separators=(",", ":"))
        output.write("\n")


def read_message(stream):
    headers = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode().split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(stream.read(int(headers["content-length"])))


def send_message(stream, message):
    payload = json.dumps(message, separators=(",", ":")).encode()
    stream.write(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
    stream.flush()


port = None
reset_first_initialize = False
reset_delay_ms = 0
for argument in sys.argv[1:]:
    if argument.startswith("--port="):
        port = int(argument.split("=", 1)[1])
    elif argument == "--reset-first-initialize":
        reset_first_initialize = True
    elif argument.startswith("--reset-delay-ms="):
        reset_delay_ms = int(argument.split("=", 1)[1])

# A Debugpy binary configured without custom args receives Zed's generated
# `--host`/`--port` vector. Make that real TCP shape itself select the delayed
# first-reset oracle so the fixture cannot silently degrade into stdio.
if port is not None:
    reset_first_initialize = True
    reset_delay_ms = 150

mode = "tcp" if port is not None else "stdio"
events = []
tcp_connection_count = 0
reset_initialize_count = 0
write_receipt(mode, events)
server = None
connection = None
if mode == "tcp":
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", port))
    server.listen(1)
    if reset_first_initialize:
        connection, _ = server.accept()
        tcp_connection_count += 1
        first_reader = connection.makefile("rb")
        first_request = read_message(first_reader)
        if first_request is None or first_request.get("command") != "initialize":
            raise RuntimeError("first TCP request was not initialize")
        reset_initialize_count += 1
        write_receipt(
            mode,
            events,
            tcp_connection_count=tcp_connection_count,
            reset_initialize_count=reset_initialize_count,
            reset_delay_ms=reset_delay_ms,
        )
        time.sleep(reset_delay_ms / 1000)
        first_reader.close()
        connection.setsockopt(
            socket.SOL_SOCKET,
            socket.SO_LINGER,
            struct.pack("ii", 1, 0),
        )
        connection.close()
    connection, _ = server.accept()
    tcp_connection_count += 1
    reader = connection.makefile("rb")
    writer = connection.makefile("wb")
else:
    reader = sys.stdin.buffer
    writer = sys.stdout.buffer

sequence = 1
while True:
    request = read_message(reader)
    if request is None:
        break
    command = request.get("command")
    if command:
        events.append(command)
        write_receipt(
            mode,
            events,
            tcp_connection_count=tcp_connection_count,
            reset_initialize_count=reset_initialize_count,
            reset_delay_ms=reset_delay_ms,
        )
    request_sequence = request.get("seq")
    if request.get("type") != "request" or request_sequence is None:
        continue
    if command == "initialize":
        body = {
            "supportsConfigurationDoneRequest": True,
            "supportsTerminateRequest": True,
        }
    elif command == "threads":
        body = {"threads": []}
    else:
        body = {}
    send_message(
        writer,
        {
            "seq": sequence,
            "type": "response",
            "request_seq": request_sequence,
            "success": True,
            "command": command,
            "body": body,
        },
    )
    sequence += 1
    if command == "initialize":
        send_message(writer, {"seq": sequence, "type": "event", "event": "initialized"})
        sequence += 1
    if command == "configurationDone":
        send_message(writer, {"seq": sequence, "type": "event", "event": "terminated"})
        sequence += 1
    if command in ("disconnect", "terminate"):
        break

if connection is not None:
    connection.close()
if server is not None:
    server.close()

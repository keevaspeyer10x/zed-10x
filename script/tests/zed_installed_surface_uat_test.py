import importlib.util
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / "zed-installed-surface-uat.py"
SPEC = importlib.util.spec_from_file_location("zed_installed_surface_uat", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
uat = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(uat)


PROJECT = "/home/keeva/uat/zed-installed-surface-test"


def raw_receipt(value):
    return {"bytes": json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n", "mode": 0o600}


def send_dap(stream, sequence, command):
    payload = json.dumps(
        {"seq": sequence, "type": "request", "command": command, "arguments": {}},
        separators=(",", ":"),
    ).encode()
    stream.write(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
    stream.flush()


def read_dap(stream):
    headers = {}
    while True:
        line = stream.readline()
        if line in (b"\r\n", b"\n"):
            break
        if not line:
            raise EOFError("DAP stream closed before a complete header")
        name, value = line.decode().split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(stream.read(int(headers["content-length"])))


def valid_observation():
    receipts = {
        "terminal": {
            "cwd": PROJECT,
            "directoryEnvironmentSha256": uat.expected_sha("directory"),
            "terminalEnvironmentSha256": uat.expected_sha("terminal"),
            "inputSha256": uat.sha256_bytes(uat.TERMINAL_INPUT),
            "stdinIsTty": True,
            "initialColumns": 80,
            "finalColumns": 120,
            "observedColumns": [80, 120, 80],
            "resizeCount": 1,
            "interrupted": True,
        },
        "task": {
            "cwd": PROJECT,
            "directoryEnvironmentSha256": uat.expected_sha("directory"),
            "taskEnvironmentSha256": uat.expected_sha("task"),
            "stdinIsTty": True,
            "interrupted": True,
        },
        "vim": {
            "cwd": PROJECT,
            "directoryEnvironmentSha256": uat.expected_sha("directory"),
            "terminalEnvironmentSha256": uat.expected_sha("terminal"),
            "inputSha256": uat.sha256_bytes(uat.VIM_INPUT),
        },
        "mcp": {
            "cwd": PROJECT,
            "environmentSha256": uat.expected_sha("mcp"),
            "events": ["initialize", "notifications/initialized", "tools/list"],
        },
        "dapStdio": {
            "cwd": PROJECT,
            "environmentSha256": uat.expected_sha("dap-stdio"),
            "events": ["initialize", "launch", "configurationDone", "terminate"],
        },
        "dapTcp": {
            "cwd": PROJECT,
            "environmentSha256": uat.expected_sha("dap-tcp"),
            "events": ["initialize", "launch", "configurationDone", "threads", "terminate"],
            "tcpConnectionCount": 2,
            "resetInitializeCount": 1,
            "resetDelayMs": 150,
        },
    }
    return {"receipts": {key: raw_receipt(value) for key, value in receipts.items()}, "processResidue": []}


class InstalledSurfaceUatTests(unittest.TestCase):
    def test_prepare_authorizes_fixture_direnv_before_opening_the_project(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = pathlib.Path(directory) / "prepare.json"
            with mock.patch.object(uat, "ssh_run", return_value=b"") as ssh_run:
                result = uat.prepare("intrepid", PROJECT, receipt)

            command = ssh_run.call_args.args[1]
            self.assertIn(f"/usr/bin/direnv allow {PROJECT}", command)
            self.assertTrue(result["directoryEnvironmentAuthorizedBeforeOpen"])
            self.assertEqual(json.loads(receipt.read_text()), result)

    def test_fixture_render_replaces_every_project_token(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = pathlib.Path(directory) / "fixture"
            manifest = uat.render_fixture(destination, PROJECT)

            self.assertIn(".zed/settings.json", manifest)
            self.assertFalse(
                any("__pycache__" in path or path.endswith(".pyc") for path in manifest)
            )
            for path in destination.rglob("*"):
                if path.is_file():
                    self.assertNotIn(uat.PROJECT_TOKEN.encode(), path.read_bytes())
            self.assertEqual(
                (destination / ".zed/settings.json").stat().st_mode & 0o777,
                0o644,
            )

            settings = json.loads((destination / ".zed/settings.json").read_text())
            debugpy = settings["dap"]["Debugpy"]
            self.assertNotIn(
                "args",
                debugpy,
                "custom Debugpy args replace its required generated host/port vector",
            )
            self.assertEqual(
                debugpy["env"],
                {"ZED_UAT_DAP_TCP_VALUE": "zed-dap-tcp-setting-v1"},
            )
            fake_dap = (destination / "fake_dap.py").read_text()
            self.assertIn("if port is not None:", fake_dap)
            self.assertIn("reset_first_initialize = True", fake_dap)
            self.assertIn("reset_delay_ms = 150", fake_dap)

    def test_tcp_fixture_uses_generated_port_and_resets_then_completes(self):
        fixture = SCRIPT.parent / "tests/fixtures/zed-installed-surface-uat/fake_dap.py"
        with tempfile.TemporaryDirectory() as directory:
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]

            environment = os.environ.copy()
            environment["ZED_UAT_DAP_TCP_VALUE"] = "zed-dap-tcp-setting-v1"
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(fixture),
                    "--host=127.0.0.1",
                    f"--port={port}",
                ],
                cwd=directory,
                env=environment,
            )
            self.addCleanup(lambda: process.poll() is None and process.kill())

            deadline = time.monotonic() + 5
            while True:
                try:
                    first = socket.create_connection(("127.0.0.1", port), timeout=1)
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        self.fail("TCP fixture did not start listening")
                    time.sleep(0.02)

            started = time.monotonic()
            with first:
                first_stream = first.makefile("rwb")
                send_dap(first_stream, 1, "initialize")
                try:
                    self.assertEqual(first.recv(1), b"")
                except ConnectionResetError:
                    pass
            self.assertGreaterEqual(time.monotonic() - started, 0.1)

            with socket.create_connection(("127.0.0.1", port), timeout=2) as second:
                second_stream = second.makefile("rwb")
                send_dap(second_stream, 2, "initialize")
                self.assertEqual(read_dap(second_stream)["command"], "initialize")
                self.assertEqual(read_dap(second_stream)["event"], "initialized")
                send_dap(second_stream, 3, "threads")
                threads = read_dap(second_stream)
                self.assertEqual(threads["command"], "threads")
                self.assertEqual(threads["body"], {"threads": []})
                send_dap(second_stream, 4, "terminate")
                self.assertEqual(read_dap(second_stream)["command"], "terminate")

            self.assertEqual(process.wait(timeout=5), 0)
            receipt = json.loads((pathlib.Path(directory) / ".uat/dap-tcp.json").read_text())
            self.assertEqual(receipt["tcpConnectionCount"], 2)
            self.assertEqual(receipt["resetInitializeCount"], 1)
            self.assertEqual(receipt["resetDelayMs"], 150)
            self.assertEqual(receipt["events"], ["initialize", "threads", "terminate"])

    def test_complete_observation_passes_with_content_free_hashes(self):
        result = uat.validate_observation(PROJECT, valid_observation())

        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["processResidue"], [])
        self.assertEqual(set(result["receiptSha256"]), set(uat.RECEIPT_FILES))

    def test_marker_level_terminal_evidence_cannot_pass(self):
        observation = valid_observation()
        terminal = json.loads(observation["receipts"]["terminal"]["bytes"])
        terminal["inputSha256"] = "0" * 64
        observation["receipts"]["terminal"] = raw_receipt(terminal)

        with self.assertRaisesRegex(uat.UatFailure, "terminal_journey_mismatch"):
            uat.validate_observation(PROJECT, observation)

    def test_process_residue_fails_cleanup(self):
        observation = valid_observation()
        observation["processResidue"] = [1234]

        with self.assertRaisesRegex(uat.UatFailure, "fixture_process_residue"):
            uat.validate_observation(PROJECT, observation)

    def test_tcp_dap_without_protocol_reconnect_cannot_pass(self):
        observation = valid_observation()
        dap = json.loads(observation["receipts"]["dapTcp"]["bytes"])
        dap["tcpConnectionCount"] = 1
        dap["resetInitializeCount"] = 0
        observation["receipts"]["dapTcp"] = raw_receipt(dap)

        with self.assertRaisesRegex(uat.UatFailure, "dapTcp_journey_mismatch"):
            uat.validate_observation(PROJECT, observation)

    def test_project_scope_is_fail_closed(self):
        with self.assertRaisesRegex(uat.UatFailure, "remote_project_outside_uat_root"):
            uat.validate_remote_project("/tmp/zed-installed-surface-test")


if __name__ == "__main__":
    unittest.main()

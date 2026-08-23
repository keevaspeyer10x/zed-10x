import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");
const canary = path.join(
  repositoryRoot,
  "script/zed-remote-environment-live-canary.py",
);

function runPython(source, args = []) {
  return spawnSync("/usr/bin/python3", ["-c", source, canary, ...args], {
    cwd: repositoryRoot,
    encoding: "utf8",
    timeout: 15_000,
  });
}

const importCanary = String.raw`
import importlib.util, pathlib, sys
path = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("zed_remote_environment_live_canary", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
`;

test("live canary compiles and exposes a bounded command-line contract", () => {
  const compile = spawnSync("/usr/bin/python3", ["-m", "py_compile", canary], {
    cwd: repositoryRoot,
    encoding: "utf8",
    timeout: 15_000,
  });
  assert.equal(compile.status, 0, compile.stderr);

  const help = spawnSync("/usr/bin/python3", [canary, "--help"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    timeout: 15_000,
  });
  assert.equal(help.status, 0, help.stderr);
  for (const option of [
    "--host",
    "--remote-server",
    "--project",
    "--sentinel",
    "--receipt",
    "--timeout",
  ]) {
    assert.match(help.stdout, new RegExp(option));
  }
});

test("private environment is framed on stdin and absent from the SSH command", () => {
  const source = `${importCanary}
import hashlib, json
secret = "private value with spaces, quotes, unicode-λ, and\\nnewlines"
frame = module.encode_environment({"ZED_LIVE_SECRET": secret})
command = module.remote_command(
    ".zed-10x-server/zed-remote-server-dev",
    "/home/keeva/repos/project with spaces",
    "env-exec-pty",
    "print('child')",
)
print(json.dumps({
    "command": command,
    "secretInCommand": secret in command,
    "secretInFrame": secret.encode() in frame,
    "frameSha256": hashlib.sha256(frame).hexdigest(),
}, sort_keys=True))
`;
  const result = runPython(source);
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.secretInCommand, false);
  assert.equal(receipt.secretInFrame, false, "JSON newlines must be escaped in the frame");
  assert.match(receipt.command, /^cd '\/home\/keeva\/repos\/project with spaces' && exec /);
  assert.match(receipt.command, /"\$HOME"\/\.zed-10x-server\/zed-remote-server-dev env-exec-pty/);
  assert.match(receipt.command, /--ready-marker zed_live_env_ready/);
  assert.doesNotMatch(receipt.command, /private value/);
  assert.match(receipt.frameSha256, /^[0-9a-f]{64}$/);
});

test("environment framing fails closed at the exact size boundary and on invalid names", () => {
  const source = `${importCanary}
import json

def outcome(value):
    try:
        module.encode_environment(value)
        return "accepted"
    except module.CanaryFailure as error:
        return str(error)

low, high = 0, module.MAX_ENVIRONMENT_BYTES + 1
while low < high:
    middle = (low + high + 1) // 2
    try:
        module.encode_environment({"BOUNDARY": "x" * middle})
        low = middle
    except module.CanaryFailure:
        high = middle - 1

accepted = module.encode_environment({"BOUNDARY": "x" * low})
print(json.dumps({
    "acceptedBytes": len(accepted),
    "next": outcome({"BOUNDARY": "x" * (low + 1)}),
    "equals": outcome({"A=B": "value"}),
    "nulName": outcome({"A\\0B": "value"}),
    "nulValue": outcome({"A": "value\\0tail"}),
}, sort_keys=True))
`;
  const result = runPython(source);
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.acceptedBytes, 1_048_585);
  assert.equal(receipt.next, "environment_too_large");
  assert.equal(receipt.equals, "invalid_environment");
  assert.equal(receipt.nulName, "invalid_environment");
  assert.equal(receipt.nulValue, "invalid_environment");
});

test("content-free terminal receipt is atomically published mode 0600", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "zed-live-env-receipt-"));
  const receiptPath = path.join(directory, "receipt.json");
  const source = `${importCanary}
import pathlib
module.write_receipt(pathlib.Path(sys.argv[2]), {
    "status": "failed",
    "failureClass": "CanaryFailure",
    "failureCode": "capability_mismatch",
    "secretPersisted": False,
})
`;
  const result = runPython(source, [receiptPath]);
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  assert.deepEqual(receipt, {
    failureClass: "CanaryFailure",
    failureCode: "capability_mismatch",
    secretPersisted: false,
    status: "failed",
  });
  assert.equal(statSync(receiptPath).mode & 0o777, 0o600);
});

test("remote port allocation sends one shell-quoted command to SSH", () => {
  const source = `${importCanary}
import json
from types import SimpleNamespace
from unittest.mock import patch

observed = {}
def fake_run(argv, **kwargs):
    observed["argv"] = argv
    return SimpleNamespace(stdout=b"43123\\n")

with patch.object(module.subprocess, "run", side_effect=fake_run):
    port = module.allocate_remote_port("ssh", "intrepid", 12.0)

print(json.dumps({"argv": observed["argv"], "port": port}, sort_keys=True))
`;
  const result = runPython(source);
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.port, 43123);
  assert.equal(receipt.argv.length, 3);
  assert.deepEqual(receipt.argv.slice(0, 2), ["ssh", "intrepid"]);
  assert.match(receipt.argv[2], /^\/usr\/bin\/python3 -c '/);
  assert.match(receipt.argv[2], /socket\.socket/);
});

test("every supported remote environment consumer uses the private transport", () => {
  const sources = {
    acp: readFileSync(path.join(repositoryRoot, "crates/agent_servers/src/acp.rs"), "utf8"),
    context: readFileSync(
      path.join(repositoryRoot, "crates/project/src/context_server_store.rs"),
      "utf8",
    ),
    dap: readFileSync(
      path.join(repositoryRoot, "crates/project/src/debugger/dap_store.rs"),
      "utf8",
    ),
    terminals: readFileSync(
      path.join(repositoryRoot, "crates/project/src/terminals.rs"),
      "utf8",
    ),
    vim: readFileSync(path.join(repositoryRoot, "crates/vim/src/command.rs"), "utf8"),
  };

  for (const consumer of [sources.acp, sources.context, sources.dap]) {
    assert.match(consumer, /build_command_with_stdin_environment\(/);
  }
  assert.match(sources.terminals, /build_command_with_stdin_environment\(/);
  assert.match(
    sources.terminals,
    /build_interactive_command_with_stdin_environment\(/,
  );

  const acpPrelude = sources.acp.indexOf("write_all(&stdin_prelude)");
  const acpProtocol = sources.acp.indexOf("AcpConnection::new");
  assert.ok(acpPrelude >= 0 && acpProtocol > acpPrelude);

  const vimPrelude = sources.vim.indexOf("write_all(&stdin_prelude)");
  const vimInput = sources.vim.indexOf("write_all(chunk.as_bytes())");
  assert.ok(vimPrelude >= 0 && vimInput > vimPrelude);
});

import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");
const canary = path.join(repositoryRoot, "script/zed-acp-live-canary.py");
const fixture = path.join(
  repositoryRoot,
  "script/tests/fixtures/fake-zed-acp-project-agent.py",
);
const fakeNpm = path.join(
  repositoryRoot,
  "script/tests/fixtures/fake-npm-exec.py",
);

function runCanary(
  mode,
  extraArgs = [],
  { ephemeral = false, preexistingSentinel = false } = {},
) {
  const project = mkdtempSync(path.join(tmpdir(), "zed-acp-project-"));
  const sentinel = path.join(project, "sentinel.txt");
  if (!ephemeral || preexistingSentinel) {
    writeFileSync(
      sentinel,
      preexistingSentinel
        ? "preexisting-project-content\n"
        : "assembled-product-evidence\nsecond-hidden-line\nthird-hidden-line\n",
    );
  }
  const output = path.join(project, `${mode}.json`);
  const childPid = path.join(project, "child.pid");
  const result = spawnSync(
    "/usr/bin/python3",
    [
      canary,
      "--surface",
      "fixture",
      "--cwd",
      project,
      "--sentinel",
      "sentinel.txt",
      ...(ephemeral ? ["--ephemeral-sentinel"] : []),
      "--output",
      output,
      "--timeout-seconds",
      mode === "timeout" ? "1" : "5",
      "--termination-grace-seconds",
      "0.1",
      "--command",
      "/usr/bin/python3",
      `--arg=${fixture}`,
      `--arg=--mode`,
      `--arg=${mode}`,
      ...(mode === "timeout"
        ? [`--arg=--child-pid`, `--arg=${childPid}`]
        : []),
      ...extraArgs,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 15_000 },
  );
  return {
    childPid,
    sentinel,
    output,
    process: result,
    receipt: JSON.parse(readFileSync(output, "utf8")),
  };
}

function runSettingsCanary(settingsText) {
  const project = mkdtempSync(path.join(tmpdir(), "zed-acp-settings-"));
  writeFileSync(path.join(project, "sentinel.txt"), "assembled-product-evidence\n");
  const settings = path.join(project, "settings.json");
  const output = path.join(project, "receipt.json");
  writeFileSync(settings, settingsText.replaceAll("FIXTURE", fixture));
  const process = spawnSync(
    "/usr/bin/python3",
    [
      canary,
      "--surface",
      "fixture-settings",
      "--cwd",
      project,
      "--sentinel",
      "sentinel.txt",
      "--output",
      output,
      "--timeout-seconds",
      "5",
      "--termination-grace-seconds",
      "0.1",
      "--settings",
      settings,
      "--endpoint",
      "Fixture",
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 15_000 },
  );
  return { process, receipt: JSON.parse(readFileSync(output, "utf8")) };
}

function runRegistryCanary() {
  const project = mkdtempSync(path.join(tmpdir(), "zed-acp-registry-"));
  writeFileSync(path.join(project, "sentinel.txt"), "assembled-product-evidence\n");
  const settings = path.join(project, "settings.json");
  const output = path.join(project, "receipt.json");
  const registryCache = path.join(project, "registry");
  mkdirSync(registryCache);
  const version = "1.2.3";
  const archive = "https://example.test/fixture.tar.gz";
  const platform = process.platform === "darwin" ? "darwin-aarch64" : "linux-x86_64";
  const versionHash = createHash("sha256").update(version).digest("hex").slice(0, 16);
  const archiveHash = createHash("sha256").update(archive).digest("hex").slice(0, 16);
  const install = path.join(
    registryCache,
    "Fixture Registry",
    `v_${version}_${versionHash}_${archiveHash}`,
  );
  mkdirSync(install, { recursive: true });
  const executable = path.join(install, "fixture-agent");
  writeFileSync(
    executable,
    `#!/bin/sh\nexec /usr/bin/python3 ${JSON.stringify(fixture)} --mode pass\n`,
  );
  chmodSync(executable, 0o700);
  writeFileSync(
    path.join(registryCache, "registry.json"),
    JSON.stringify({
      agents: [
        {
          id: "Fixture Registry",
          version,
          distribution: {
            binary: {
              [platform]: { archive, cmd: "./fixture-agent" },
            },
          },
        },
      ],
    }),
  );
  writeFileSync(
    settings,
    JSON.stringify({
      agent_servers: { "Fixture Registry": { type: "registry" } },
    }),
  );
  const processResult = spawnSync(
    "/usr/bin/python3",
    [
      canary,
      "--surface",
      "fixture-registry",
      "--cwd",
      project,
      "--sentinel",
      "sentinel.txt",
      "--output",
      output,
      "--timeout-seconds",
      "5",
      "--termination-grace-seconds",
      "0.1",
      "--settings",
      settings,
      "--endpoint",
      "Fixture Registry",
      "--registry-cache",
      registryCache,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 15_000 },
  );
  return {
    process: processResult,
    receipt: JSON.parse(readFileSync(output, "utf8")),
  };
}

function runRegistryNpxCanary() {
  const project = mkdtempSync(path.join(tmpdir(), "zed-acp-registry-npx-"));
  writeFileSync(path.join(project, "sentinel.txt"), "assembled-product-evidence\n");
  const settings = path.join(project, "settings.json");
  const output = path.join(project, "receipt.json");
  const registryCache = path.join(project, "registry");
  mkdirSync(registryCache);
  const npmCommand = path.join(project, "fake-npm");
  writeFileSync(
    npmCommand,
    `#!/bin/sh\nFAKE_ACP_FIXTURE=${JSON.stringify(fixture)} exec /usr/bin/python3 ${JSON.stringify(fakeNpm)} "$@"\n`,
  );
  chmodSync(npmCommand, 0o700);
  writeFileSync(
    path.join(registryCache, "registry.json"),
    JSON.stringify({
      agents: [
        {
          id: "Fixture Npx",
          version: "1.2.3",
          distribution: { npx: { package: "fixture-acp@1.2.3" } },
        },
      ],
    }),
  );
  writeFileSync(
    settings,
    JSON.stringify({ agent_servers: { "Fixture Npx": { type: "registry" } } }),
  );
  const processResult = spawnSync(
    "/usr/bin/python3",
    [
      canary,
      "--surface",
      "fixture-registry-npx",
      "--cwd",
      project,
      "--sentinel",
      "sentinel.txt",
      "--output",
      output,
      "--timeout-seconds",
      "5",
      "--termination-grace-seconds",
      "0.1",
      "--settings",
      settings,
      "--endpoint",
      "Fixture Npx",
      "--registry-cache",
      registryCache,
      "--npm-command",
      npmCommand,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 15_000 },
  );
  return {
    process: processResult,
    receipt: JSON.parse(readFileSync(output, "utf8")),
  };
}

test("project-aware ACP canary accepts a completed tool call with exact project evidence", () => {
  const result = runCanary("pass");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.toolCallCompleted, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
  assert.equal(result.receipt.processGroupGone, true);
  assert.equal(result.receipt.closeSessionSupported, true);
  assert.equal(result.receipt.closeSessionCompleted, true);
  assert.equal(result.receipt.promptOrResponseContentRetained, false);
});

test("project-aware ACP canary accepts byte-faithful numbered read output", () => {
  for (const mode of [
    "pass-numbered-tab",
    "pass-numbered-tab-compact",
    "pass-numbered-arrow",
    "pass-numbered-arrow-compact",
  ]) {
    const result = runCanary(mode);
    assert.equal(result.process.status, 0, result.process.stderr);
    assert.equal(result.receipt.status, "pass");
    assert.equal(result.receipt.toolInputSentinelMatched, true);
    assert.equal(result.receipt.toolOutputSentinelMatched, true);
    assert.equal(result.receipt.toolEvidenceMatched, true);
    assert.match(result.receipt.toolEvidenceFormat, /^numbered_/);
  }
});

test("standard ACP locations can bind the exact project read without rawInput", () => {
  const result = runCanary("pass-location-only");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.toolInputSentinelMatched, false);
  assert.equal(result.receipt.toolLocationSentinelMatched, true);
  assert.equal(result.receipt.toolPathSentinelMatched, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.toolEvidenceFormat, "exact");
});

test("a wrong ACP location cannot borrow exact output as project evidence", () => {
  const result = runCanary("wrong-location-only");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.toolInputSentinelMatched, false);
  assert.equal(result.receipt.toolLocationSentinelMatched, false);
  assert.equal(result.receipt.toolPathSentinelMatched, false);
  assert.equal(result.receipt.toolOutputSentinelMatched, true);
});

test("output without path metadata cannot authorize a static project sentinel", () => {
  const result = runCanary("pass-output-only");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.toolPathSentinelMatched, false);
  assert.equal(result.receipt.toolOutputSentinelMatched, true);
});

test("exact output proves a prompt-hidden ephemeral project sentinel", () => {
  const result = runCanary("pass-output-only", [], { ephemeral: true });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.ephemeralSentinel, true);
  assert.equal(result.receipt.sentinelCreated, true);
  assert.equal(result.receipt.sentinelRemoved, true);
  assert.equal(result.receipt.toolPathSentinelMatched, false);
  assert.equal(result.receipt.toolOutputSentinelMatched, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.toolEvidenceBasis, "ephemeral_output");
  assert.equal(result.receipt.promptOrResponseContentRetained, false);
  assert.equal(existsSync(result.sentinel), false);
});

test("ephemeral canary refuses an existing project path without overwriting it", () => {
  const result = runCanary(
    "pass-output-only",
    [],
    { ephemeral: true, preexistingSentinel: true },
  );
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "sentinel_collision");
  assert.equal(result.receipt.processStarted, false);
  assert.equal(result.receipt.sentinelCreated, false);
  assert.equal(result.receipt.sentinelRemoved, false);
  assert.equal(readFileSync(result.sentinel, "utf8"), "preexisting-project-content\n");
});

test("ephemeral cleanup refuses to delete a route-replaced sentinel", () => {
  const result = runCanary("replace-sentinel", [], { ephemeral: true });
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "sentinel_cleanup_failed");
  assert.equal(result.receipt.sentinelCreated, true);
  assert.equal(result.receipt.sentinelRemoved, false);
  assert.equal(readFileSync(result.sentinel, "utf8"), "replacement-owned-by-agent\n");
});

test("ephemeral cleanup never unlinks while route cleanup is unproven", () => {
  const probe = spawnSync(
    "/usr/bin/python3",
    [
      "-c",
      String.raw`
import importlib.util
import pathlib
import tempfile

path = pathlib.Path(${JSON.stringify(canary)})
spec = importlib.util.spec_from_file_location("zed_acp_live_canary", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

with tempfile.TemporaryDirectory() as root:
    root_path = pathlib.Path(root)
    sentinel_path = root_path / "sentinel.txt"
    owned = module.OwnedEphemeralSentinel(root_path, pathlib.Path("sentinel.txt"))
    assert module.release_owned_sentinel(owned, False) is False
    assert sentinel_path.is_file()
    assert owned.file_fd == -1
    assert owned.directory_fd == -1
`,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 5_000 },
  );
  assert.equal(probe.status, 0, probe.stderr);
});

test("project-aware journey succeeds when the agent does not advertise session close", () => {
  const result = runCanary("pass-without-close");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.toolCallCompleted, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
  assert.equal(result.receipt.closeSessionSupported, false);
  assert.equal(result.receipt.closeSessionCompleted, false);
  assert.equal(result.receipt.processGroupGone, true);
});

test("unrelated completed tool calls cannot be combined into project evidence", () => {
  const result = runCanary("split-evidence");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.status, "failed");
  assert.equal(result.receipt.toolCallCompleted, true);
  assert.equal(result.receipt.toolEvidenceMatched, false);
  assert.equal(result.receipt.terminalMarkerObserved, true);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.processGroupGone, true);
});

test("ACP client read capability proves the real project without agent-supplied tool output", () => {
  const result = runCanary("client-read");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.clientReadRequestCount, 1);
  assert.equal(result.receipt.clientReadCompletedCount, 1);
  assert.equal(result.receipt.clientReadSentinelMatched, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
});

test("ACP client read capability mirrors Zed resource-not-found responses and continues", () => {
  const result = runCanary("client-read-missing-after-sentinel");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.clientReadRequestCount, 2);
  assert.equal(result.receipt.clientReadCompletedCount, 1);
  assert.equal(result.receipt.clientReadErrorResponseCount, 1);
  assert.equal(result.receipt.clientReadSentinelMatched, true);
  assert.equal(result.receipt.clientReadFailureReason, null);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
});

test("ACP client read capability refuses paths outside the test project", () => {
  const result = runCanary("client-read-outside");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "tool_evidence_missing");
  assert.equal(result.receipt.clientReadFailureReason, null);
  assert.equal(result.receipt.clientReadRequestCount, 1);
  assert.equal(result.receipt.clientReadCompletedCount, 0);
  assert.equal(result.receipt.clientReadErrorResponseCount, 1);
  assert.equal(result.receipt.clientReadOutsideProjectDeniedCount, 1);
  assert.equal(result.receipt.clientReadSentinelMatched, false);
  assert.equal(result.receipt.processGroupGone, true);
});

test("a denied global read does not erase exact project evidence", () => {
  const result = runCanary("client-read-outside-after-sentinel");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.clientReadRequestCount, 2);
  assert.equal(result.receipt.clientReadCompletedCount, 1);
  assert.equal(result.receipt.clientReadErrorResponseCount, 1);
  assert.equal(result.receipt.clientReadOutsideProjectDeniedCount, 1);
  assert.equal(result.receipt.clientReadSentinelMatched, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
  assert.equal(result.receipt.processGroupGone, true);
});

test("ACP client read diagnostics classify invalid requests without retaining paths", () => {
  const result = runCanary("client-read-relative");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "invalid_client_read_request");
  assert.equal(result.receipt.clientReadFailureReason, "path_not_absolute");
  assert.equal(result.receipt.clientReadRequestCount, 1);
  assert.equal(result.receipt.clientReadCompletedCount, 0);
  assert.equal(result.receipt.sentinelPath, undefined);
  assert.equal(result.receipt.processGroupGone, true);
});

test("ACP client terminal capability proves an exact read-only project journey", () => {
  const result = runCanary("client-terminal-read");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.clientTerminalRequestCount, 4);
  assert.equal(result.receipt.clientTerminalReadCompletedCount, 1);
  assert.equal(result.receipt.clientTerminalSentinelMatched, true);
  assert.equal(result.receipt.clientTerminalFailureReason, null);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.toolEvidenceBasis, "client_terminal_exact");
  assert.equal(result.receipt.terminalMarkerObserved, true);
});

test("ACP client terminal capability refuses a mutating command without executing it", () => {
  const result = runCanary("client-terminal-write");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "terminal_command_not_read_only");
  assert.equal(result.receipt.clientTerminalRequestCount, 1);
  assert.equal(result.receipt.clientTerminalReadCompletedCount, 0);
  assert.equal(result.receipt.clientTerminalSentinelMatched, false);
  assert.equal(result.receipt.clientTerminalFailureReason, "command_not_allowed");
  assert.equal(result.receipt.processGroupGone, true);
});

test("ACP client terminal capability refuses environment injection as policy", () => {
  const result = runCanary("client-terminal-environment");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "terminal_command_not_read_only");
  assert.equal(result.receipt.clientTerminalFailureReason, "environment_not_allowed");
  assert.equal(result.receipt.processGroupGone, true);
});

test("ACP client terminal truncation cannot satisfy exact project evidence", () => {
  const result = runCanary("client-terminal-truncated");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.clientTerminalReadCompletedCount, 1);
  assert.equal(result.receipt.clientTerminalSentinelMatched, false);
  assert.equal(result.receipt.toolEvidenceMatched, false);
  assert.equal(result.receipt.processGroupGone, true);
});

test("optional vendor client requests receive method-not-found and do not block the journey", () => {
  const result = runCanary("optional-client-extension");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.unsupportedClientRequestCount, 1);
  assert.deepEqual(
    result.receipt.unsupportedClientMethodSha256s,
    [createHash("sha256").update("cursor/update_todos").digest("hex")],
  );
  assert.equal(result.receipt.clientReadSentinelMatched, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
});

test("unsupported standard client requests fail rather than hide mutation attempts", () => {
  const result = runCanary("optional-client-reserved-write");
  assert.equal(result.process.status, 1, result.process.stderr);
  assert.equal(result.receipt.failureClass, "unsupported_client_request");
  assert.equal(result.receipt.unsupportedClientRequestCount, 0);
  assert.deepEqual(result.receipt.unsupportedClientMethodSha256s, []);
  assert.equal(readFileSync(result.sentinel, "utf8").includes("must-not-be-written"), false);
  assert.equal(result.receipt.processGroupGone, true);
});

test("production JSONC settings resolve the configured endpoint", () => {
  const result = runSettingsCanary(`{
    // Zed settings allow comments and trailing commas.
    "agent_servers": {
      "Fixture": {
        "type": "custom",
        "command": "/usr/bin/python3",
        "args": ["FIXTURE", "--mode", "pass"],
      },
    },
  }`);
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.endpoint, "Fixture");
});

test("installed registry artifact resolves through the same versioned cache identity", () => {
  const result = runRegistryCanary();
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.endpoint, "Fixture Registry");
  assert.equal(result.receipt.processGroupGone, true);
});

test("registry npx route preserves the product npm-exec boundary", () => {
  const result = runRegistryNpxCanary();
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.endpoint, "Fixture Npx");
  assert.equal(result.receipt.processGroupGone, true);
});

test("malformed JSONC settings fail closed", () => {
  const result = runSettingsCanary(`{
    "agent_servers": {}
  } /* unterminated`);
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "invalid_settings_jsonc");
  assert.equal(result.receipt.processStarted, false);
});

test("marker-only output cannot impersonate a project-aware journey", () => {
  const result = runCanary("marker-only");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.status, "failed");
  assert.equal(result.receipt.failureClass, "tool_evidence_missing");
  assert.equal(result.receipt.stopReason, "end_turn");
  assert.equal(result.receipt.closeSessionSupported, true);
  assert.equal(result.receipt.closeSessionCompleted, true);
});

test("echoing every prompt-visible value cannot impersonate a project read", () => {
  const result = runCanary("prompt-echo");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.closeSessionSupported, true);
  assert.equal(result.receipt.closeSessionCompleted, true);
});

test("tool output from the wrong project is rejected", () => {
  const result = runCanary("wrong-cwd");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.closeSessionSupported, true);
  assert.equal(result.receipt.closeSessionCompleted, true);
});

test("failed cleanup never masks the original project failure", () => {
  const result = runCanary("wrong-cwd-close-error");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
  assert.equal(result.receipt.toolCallCompleted, true);
  assert.equal(result.receipt.closeSessionSupported, true);
  assert.equal(result.receipt.closeSessionCompleted, false);
  assert.equal(result.receipt.processGroupGone, true);
});

test("missing executable is classified before a false-green session", () => {
  const result = runCanary("pass", ["--command", "/definitely/missing/zed-acp-agent"]);
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "missing_executable");
  assert.equal(result.receipt.processStarted, false);
});

test("authentication and capacity failures remain distinct", () => {
  const authentication = runCanary("authentication");
  assert.equal(authentication.process.status, 1);
  assert.equal(authentication.receipt.failureClass, "authentication_expired");

  const authenticationMessage = runCanary("authentication-message");
  assert.equal(authenticationMessage.process.status, 1);
  assert.equal(
    authenticationMessage.receipt.failureClass,
    "authentication_required",
  );
  assert.equal(
    authenticationMessage.receipt.agentMessageClassification,
    "authentication_required",
  );
  assert.match(authenticationMessage.receipt.agentMessageSha256, /^[0-9a-f]{64}$/);
  assert.equal(
    JSON.stringify(authenticationMessage.receipt).includes("Please login"),
    false,
  );

  const capacity = runCanary("capacity");
  assert.equal(capacity.process.status, 1);
  assert.equal(capacity.receipt.failureClass, "capacity_or_rate_limit");

  const sessionLimit = runCanary("session-limit");
  assert.equal(sessionLimit.process.status, 1);
  assert.equal(sessionLimit.receipt.failureClass, "capacity_or_rate_limit");

  const weeklyLimit = runCanary("weekly-limit");
  assert.equal(weeklyLimit.process.status, 1);
  assert.equal(weeklyLimit.receipt.failureClass, "capacity_or_rate_limit");
});

test("provider error evidence remains content-free and classifies API-key failures", () => {
  const probe = spawnSync(
    "/usr/bin/python3",
    [
      "-c",
      String.raw`
import importlib.util
import pathlib

path = pathlib.Path(${JSON.stringify(canary)})
spec = importlib.util.spec_from_file_location("zed_acp_live_canary", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

assert module.classify_error("API key required") == "authentication_required"
assert module.classify_error("access token missing") == "authentication_required"
`,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 5_000 },
  );
  assert.equal(probe.status, 0, probe.stderr);
});

test("read-only canary never grants route-requested permissions", () => {
  for (const mode of ["permission-write", "permission-shell", "permission-unknown"]) {
    const result = runCanary(mode);
    assert.equal(result.process.status, 1, `${mode}: ${result.process.stderr}`);
    assert.equal(result.receipt.failureClass, "permission_requested", mode);
    assert.equal(result.receipt.permissionRequestsApproved, 0, mode);
    assert.equal(result.receipt.processGroupGone, true, mode);
  }
});

test("timeout kills the owned ACP process group", async () => {
  const result = runCanary("timeout");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "timeout");
  assert.equal(result.receipt.processGroupGone, true);
  const childPid = Number(readFileSync(result.childPid, "utf8").trim());
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.throws(() => process.kill(childPid, 0), { code: "ESRCH" });
});

test("a reused or inaccessible process-group id fails closed without losing evidence", () => {
  const probe = spawnSync(
    "/usr/bin/python3",
    [
      "-c",
      String.raw`
import importlib.util
import pathlib
import signal

path = pathlib.Path(${JSON.stringify(canary)})
spec = importlib.util.spec_from_file_location("zed_acp_live_canary", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

def denied(_process_group, _signal_number):
    raise PermissionError(1, "operation not permitted")

module.os.killpg = denied
assert module.try_signal_process_group(424242, signal.SIGKILL) is False
`,
    ],
    { cwd: repositoryRoot, encoding: "utf8", timeout: 5_000 },
  );
  assert.equal(probe.status, 0, probe.stderr);
});

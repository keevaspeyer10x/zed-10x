import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
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

function runCanary(mode, extraArgs = []) {
  const project = mkdtempSync(path.join(tmpdir(), "zed-acp-project-"));
  writeFileSync(path.join(project, "sentinel.txt"), "assembled-product-evidence\n");
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

test("project evidence may be established across multiple completed tool calls", () => {
  const result = runCanary("split-evidence");
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.equal(result.receipt.status, "pass");
  assert.equal(result.receipt.toolCallCompleted, true);
  assert.equal(result.receipt.toolEvidenceMatched, true);
  assert.equal(result.receipt.terminalMarkerObserved, true);
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
});

test("echoing every prompt-visible value cannot impersonate a project read", () => {
  const result = runCanary("prompt-echo");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
});

test("tool output from the wrong project is rejected", () => {
  const result = runCanary("wrong-cwd");
  assert.equal(result.process.status, 1);
  assert.equal(result.receipt.failureClass, "project_evidence_mismatch");
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

  const capacity = runCanary("capacity");
  assert.equal(capacity.process.status, 1);
  assert.equal(capacity.receipt.failureClass, "capacity_or_rate_limit");
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

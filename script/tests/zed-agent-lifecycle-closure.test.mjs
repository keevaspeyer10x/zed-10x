import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");

function readJson(relativePath) {
  return JSON.parse(readFileSync(path.join(repositoryRoot, relativePath), "utf8"));
}

const discovery = readJson("docs/discovery-ir.json");
const plan = readJson("docs/test-plan.json");
const implementation = readJson("docs/test-implementation-map.json");
const summary = readJson("docs/test-results/summary.json");

test("plan generation never overwrites completed execution evidence", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "zed-plan-preserves-evidence-"));
  try {
    mkdirSync(path.join(root, "docs/test-plan-inputs"), { recursive: true });
    mkdirSync(path.join(root, "docs/test-results"), { recursive: true });
    writeFileSync(
      path.join(root, "docs/test-plan-inputs/zed-agent-picker-inventory.json"),
      readFileSync(
        path.join(repositoryRoot, "docs/test-plan-inputs/zed-agent-picker-inventory.json"),
      ),
    );
    const executionSummary = {
      schemaVersion: 3,
      decisionCandidate: "production_ready",
      executedRows: 5,
      evidenceBinding: { testedRevision: "a".repeat(40) },
    };
    const implementationMap = {
      implementedRows: 5,
      executedRows: 5,
      mappings: [{ id: "executed-row", outcome: "passed" }],
    };
    const lifecycleState = {
      generatedAt: "2026-09-02T00:00:00Z",
      lifecycle: [{ id: "executed-row", status: "passed" }],
    };
    writeFileSync(
      path.join(root, "docs/test-results/summary.json"),
      `${JSON.stringify(executionSummary, null, 2)}\n`,
    );
    writeFileSync(
      path.join(root, "docs/test-implementation-map.json"),
      `${JSON.stringify(implementationMap, null, 2)}\n`,
    );
    writeFileSync(
      path.join(root, "docs/test-lifecycle-state.json"),
      `${JSON.stringify(lifecycleState, null, 2)}\n`,
    );

    const result = spawnSync(
      process.execPath,
      [path.join(repositoryRoot, "script/generate-zed-agent-session-plan.mjs")],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          SOURCE_DATE_EPOCH: "0",
          ZED_TEST_PLAN_ROOT: root,
        },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(
      JSON.parse(readFileSync(path.join(root, "docs/test-results/summary.json"), "utf8")),
      executionSummary,
    );
    assert.deepEqual(
      JSON.parse(readFileSync(path.join(root, "docs/test-implementation-map.json"), "utf8")),
      implementationMap,
    );
    assert.deepEqual(
      JSON.parse(readFileSync(path.join(root, "docs/test-lifecycle-state.json"), "utf8")),
      lifecycleState,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fresh lifecycle closes the exact installed External Agents surfaces and journeys", () => {
  assert.equal(discovery.schemaVersion, 3);
  assert.equal(discovery.coverageClosureVersion, 2);
  assert.equal(discovery.freshLifecycle, true);
  assert.deepEqual(
    discovery.productSurfaces.map(({ id }) => id),
    [
      "SURFACE-ACP-MAC-LOCAL",
      "SURFACE-ACP-INTREPID-PERSISTENT",
      "SURFACE-ACP-INTREPID-ORDINARY",
    ],
  );
  assert.deepEqual(
    discovery.criticalJourneys.map(({ id }) => id),
    [
      "JOURNEY-HOST-SCOPED-INVENTORY",
      "JOURNEY-NEW-SESSION-PROJECT-OUTCOME",
      "JOURNEY-TERMINATION-CLEANUP",
      "JOURNEY-PERSISTENT-SESSION-RECOVERY",
      "JOURNEY-AGENT-SWITCH-AND-RETURN",
    ],
  );
  assert.equal(discovery.surfaceJourneyMatrix.length, 15);
  assert.equal(
    discovery.surfaceJourneyMatrix.filter(({ applicability }) => applicability === "applicable")
      .length,
    14,
  );
  assert.equal(discovery.selectableVariants.length, 22);
  assert.equal(plan.variantJourneyCoverage.cells.length, 72);
  assert.equal(
    plan.variantJourneyCoverage.cells.filter(({ coverageMode }) => coverageMode === "direct").length,
    72,
  );
  assert.equal(plan.tests.length, 5);

  const rowsById = new Map(plan.tests.map((row) => [row.id, row]));
  for (const cell of plan.variantJourneyCoverage.cells) {
    const variant = discovery.selectableVariants.find(({ id }) => id === cell.variantId);
    assert.ok(variant, `${cell.variantId} exists`);
    assert.equal(cell.coverageMode, "direct");
    assert.equal(cell.evidenceLayer, "assembled_product");
    assert.equal(cell.rowIds.length, 1);
    const row = rowsById.get(cell.rowIds[0]);
    assert.ok(row, `${cell.variantId} ${cell.journeyId} has a plan row`);
    assert.ok(row.variantIds.includes(cell.variantId));
    assert.ok(row.journeyIds.includes(cell.journeyId));
    assert.ok(row.surfaceIds.includes(variant.surfaceId));
  }

  assert.deepEqual(rowsById.get("ZED-ACP-RECOVERY-004").variantIds, [
    "VAR-INTREPID-CODEX-PRIMARY",
    "VAR-INTREPID-CURSOR",
  ]);
  assert.deepEqual(rowsById.get("ZED-ACP-SWITCH-005").variantIds, [
    "VAR-MAC-CODEX",
    "VAR-MAC-CURSOR",
    "VAR-INTREPID-CODEX-SECONDARY",
    "VAR-INTREPID-CURSOR",
  ]);
});

test("representative variants bind their stateful recovery and switch journeys", () => {
  const variants = new Map(
    discovery.selectableVariants.map((variant) => [variant.id, variant]),
  );
  for (const id of ["VAR-INTREPID-CODEX-PRIMARY", "VAR-INTREPID-CURSOR"]) {
    assert.ok(variants.get(id).journeyIds.includes("JOURNEY-PERSISTENT-SESSION-RECOVERY"), id);
  }
  for (const id of [
    "VAR-MAC-CODEX",
    "VAR-MAC-CURSOR",
    "VAR-INTREPID-CODEX-SECONDARY",
    "VAR-INTREPID-CURSOR",
  ]) {
    assert.ok(variants.get(id).journeyIds.includes("JOURNEY-AGENT-SWITCH-AND-RETURN"), id);
  }

  const switchRow = plan.tests.find(({ id }) => id === "ZED-ACP-SWITCH-005");
  const receipts = [
    readJson("docs/test-results/zed-acp-switch-mac.json"),
    readJson("docs/test-results/zed-acp-switch-intrepid.json"),
  ];
  const receiptVariantIds = new Set();
  for (const receipt of receipts) {
    assert.equal(receipt.id, switchRow.id);
    assert.equal(receipt.status, "passed");
    assert.ok(receipt.executedRoutes.length > 0);
    for (const route of receipt.executedRoutes) {
      const variant = variants.get(route.variantId);
      assert.ok(variant, `${route.variantId} exists`);
      assert.equal(route.displayName, variant.name);
      assert.ok(switchRow.variantIds.includes(route.variantId));
      assert.ok(
        receipt.journey.some((step) => step.startsWith(route.displayName)),
        `${route.displayName} appears in the observed journey`,
      );
      receiptVariantIds.add(route.variantId);
    }
  }
  assert.deepEqual([...receiptVariantIds].sort(), [...switchRow.variantIds].sort());
});

test("every surface, variant, and requirement source is an existing regular repository file", () => {
  const sources = new Set([
    ...discovery.requirements.flatMap(({ source }) => source.split(";").map((path) => path.trim())),
    ...discovery.productSurfaces.flatMap(({ sourceRefs }) => sourceRefs),
    ...discovery.selectableVariants.map(({ sourceRef }) => sourceRef),
  ]);
  assert.ok(sources.size > 0);
  for (const relativePath of sources) {
    assert.equal(path.isAbsolute(relativePath), false, relativePath);
    assert.equal(relativePath.split(path.sep).includes(".."), false, relativePath);
    assert.equal(statSync(path.join(repositoryRoot, relativePath)).isFile(), true, relativePath);
    const tracked = spawnSync(
      "git",
      ["-C", repositoryRoot, "ls-files", "--error-unmatch", "--", relativePath],
      { encoding: "utf8" },
    );
    assert.equal(tracked.status, 0, `${relativePath} must be tracked`);
  }
});

test("recorded production closure retains a complete immutable digest manifest", () => {
  assert.equal(summary.decisionCandidate, "production_ready");
  assert.equal(implementation.plannedRows, 5);
  assert.equal(implementation.readyNowRows, 5);
  assert.equal(implementation.implementedRows, 5);
  assert.equal(implementation.executedRows, 5);
  assert.equal(implementation.remainingRows, 0);
  assert.equal(summary.executedRows, 5);
  assert.equal(summary.remainingPlannedRows, 0);

  const revision = summary.evidenceBinding?.testedRevision;
  assert.match(revision ?? "", /^[0-9a-f]{40}$/);

  const boundInputs = new Map(
    summary.evidenceBinding.loadBearingInputs.map((entry) => [entry.path, entry.sha256]),
  );
  assert.equal(
    boundInputs.size,
    summary.evidenceBinding.loadBearingInputs.length,
    "recorded load-bearing paths must be unique",
  );
  const requiredSources = new Set([
    ...discovery.productSurfaces.flatMap(({ sourceRefs }) => sourceRefs),
    ...discovery.selectableVariants.map(({ sourceRef }) => sourceRef),
  ]);
  for (const relativePath of requiredSources) {
    assert.ok(boundInputs.has(relativePath), `${relativePath} must have a recorded digest`);
  }
  for (const [relativePath, digest] of boundInputs) {
    assert.equal(path.isAbsolute(relativePath), false, relativePath);
    assert.equal(relativePath.split(path.sep).includes(".."), false, relativePath);
    assert.match(digest, /^[0-9a-f]{64}$/, relativePath);
  }
});

test("focused CI executes every containment and alias-policy regression", () => {
  const workflow = readFileSync(
    path.join(repositoryRoot, ".github/workflows/zed-10x-ci.yml"),
    "utf8",
  );
  for (const testName of [
    "unix::env_exec_reaps_its_command_group_when_supervisor_dies_before_identity_report",
    "unix::env_exec_reaps_its_command_group_when_guardian_dies_before_readiness_report",
    "unix::env_exec_reaps_its_command_group_when_guardian_dies_after_transport_eof",
    "unix::env_exec_preserves_the_command_exit_identity",
    "test_legacy_alias_resolves_dedicated_policy_before_connection_selection",
    "cargo test --locked -p acp_thread --lib",
    "test_retry_load_refreshes_dedicated_transport_and_preserves_session_id",
    "test_drop_shuts_down_dedicated_transport",
    "test_drop_uses_connection_policy_captured_at_acquisition",
    "test_state_transition_uses_connection_policy_captured_at_acquisition",
    "test_drop_closes_dedicated_sessions_before_transport_shutdown",
    "env_exec_flushes_partial_protocol_frames_before_command_exit",
    "env_exec_preserves_known_exit_identity_after_child_closes_stdin",
    "env_exec_can_start_its_guardian_after_its_binary_is_unlinked",
    "env_exec_does_not_hang_on_an_escaped_descendant_holding_output_open",
  ]) {
    assert.ok(workflow.includes(testName), testName);
  }
});

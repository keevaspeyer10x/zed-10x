import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");

function readJson(relativePath) {
  return JSON.parse(readFileSync(path.join(repositoryRoot, relativePath), "utf8"));
}

function sha256(relativePath) {
  return createHash("sha256")
    .update(readFileSync(path.join(repositoryRoot, relativePath)))
    .digest("hex");
}

function sha256AtRevision(revision, relativePath) {
  const source = spawnSync(
    "git",
    ["-C", repositoryRoot, "show", `${revision}:${relativePath}`],
    { maxBuffer: 16 * 1024 * 1024 },
  );
  assert.equal(source.status, 0, `${relativePath} must exist at ${revision}`);
  return createHash("sha256").update(source.stdout).digest("hex");
}

const discovery = readJson("docs/discovery-ir.json");
const plan = readJson("docs/test-plan.json");
const implementation = readJson("docs/test-implementation-map.json");
const summary = readJson("docs/test-results/summary.json");

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
  assert.equal(plan.variantJourneyCoverage.cells.length, 66);
  assert.equal(
    plan.variantJourneyCoverage.cells.filter(({ coverageMode }) => coverageMode === "direct").length,
    66,
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
    "VAR-INTREPID-CODEX-PRIMARY",
    "VAR-INTREPID-CURSOR",
  ]);
});

test("every surface and variant source is an existing regular repository file", () => {
  const sources = new Set([
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

test("production closure is bound to unchanged installed load-bearing bytes", () => {
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
  const ancestor = spawnSync(
    "git",
    ["-C", repositoryRoot, "merge-base", "--is-ancestor", revision, "HEAD"],
    { encoding: "utf8" },
  );
  assert.equal(ancestor.status, 0, `${revision} must be an ancestor of HEAD`);

  const boundInputs = new Map(
    summary.evidenceBinding.loadBearingInputs.map((entry) => [entry.path, entry.sha256]),
  );
  const requiredSources = new Set([
    ...discovery.productSurfaces.flatMap(({ sourceRefs }) => sourceRefs),
    ...discovery.selectableVariants.map(({ sourceRef }) => sourceRef),
  ]);
  for (const relativePath of requiredSources) {
    assert.equal(boundInputs.get(relativePath), sha256(relativePath), relativePath);
  }
  for (const [relativePath, digest] of boundInputs) {
    assert.equal(sha256AtRevision(revision, relativePath), digest, relativePath);
    assert.equal(sha256(relativePath), digest, relativePath);
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
  ]) {
    assert.ok(workflow.includes(testName), testName);
  }
});

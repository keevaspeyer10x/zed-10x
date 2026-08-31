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
    ],
  );
  assert.equal(discovery.surfaceJourneyMatrix.length, 12);
  assert.equal(
    discovery.surfaceJourneyMatrix.filter(({ applicability }) => applicability === "applicable")
      .length,
    10,
  );
  assert.equal(discovery.selectableVariants.length, 22);
  assert.equal(plan.variantJourneyCoverage.cells.length, 74);
  assert.equal(plan.tests.length, 4);

  const rowsById = new Map(plan.tests.map((row) => [row.id, row]));
  for (const cell of plan.variantJourneyCoverage.cells) {
    assert.equal(cell.coverageMode, "direct");
    assert.equal(cell.evidenceLayer, "assembled_product");
    assert.equal(cell.rowIds.length, 1);
    const row = rowsById.get(cell.rowIds[0]);
    const variant = discovery.selectableVariants.find(({ id }) => id === cell.variantId);
    assert.ok(row, `${cell.variantId} ${cell.journeyId} has a plan row`);
    assert.ok(variant, `${cell.variantId} exists`);
    assert.ok(row.variantIds.includes(cell.variantId));
    assert.ok(row.journeyIds.includes(cell.journeyId));
    assert.ok(row.surfaceIds.includes(variant.surfaceId));
  }
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

test("production closure is bound to an ancestor revision and current load-bearing bytes", () => {
  assert.equal(summary.decisionCandidate, "production_ready");
  assert.equal(implementation.plannedRows, 4);
  assert.equal(implementation.readyNowRows, 4);
  assert.equal(implementation.implementedRows, 4);
  assert.equal(implementation.executedRows, 4);
  assert.equal(implementation.remainingRows, 0);
  assert.equal(summary.executedRows, 4);
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
    assert.equal(sha256(relativePath), digest, relativePath);
  }
});

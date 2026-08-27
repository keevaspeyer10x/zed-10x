import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");
const aggregate = path.join(repositoryRoot, "script/zed-agent-matrix-aggregate.py");

function sha256(pathname) {
  return createHash("sha256").update(readFileSync(pathname)).digest("hex");
}

function fixture({ mutateSummary, mutateReceipt, mutateFixture } = {}) {
  const root = mkdtempSync(path.join(tmpdir(), "zed-matrix-aggregate-"));
  const inventory = path.join(root, "inventory.json");
  const sourceManifest = path.join(root, "manifest.json");
  writeFileSync(
    inventory,
    JSON.stringify({
      schema: "zed-agent-picker-inventory-v1",
      surfaces: { "mac-local": ["Mac Route"], intrepid: ["Intrepid Route"] },
    }),
  );
  writeFileSync(sourceManifest, "{}\n");
  const inventorySha = sha256(inventory);
  const manifestSha = sha256(sourceManifest);
  const surfaces = {};
  for (const [surface, endpoint] of [
    ["mac-local", "Mac Route"],
    ["intrepid", "Intrepid Route"],
  ]) {
    const receipts = path.join(root, `${surface}-receipts`);
    mkdirSync(receipts, { mode: 0o700 });
    chmodSync(receipts, 0o700);
    const receiptPath = path.join(receipts, `01-${endpoint.replaceAll(" ", "-")}.json`);
    const receipt = {
      schema: "zed-acp-project-canary-v1",
      surface,
      endpoint,
      status: "pass",
      failureClass: null,
      processGroupGone: true,
      promptOrResponseContentRetained: false,
      permissionRequestsApproved: 0,
      elapsedMs: 1,
    };
    mutateReceipt?.(surface, receipt);
    writeFileSync(receiptPath, `${JSON.stringify(receipt)}\n`);
    const summary = {
      schema: "zed-agent-picker-uat-v1",
      status: "pass",
      surface,
      failureClass: null,
      expectedEndpoints: [endpoint],
      configuredManagedEndpoints: [endpoint],
      inventorySha256: inventorySha,
      sourceManifestSha256: manifestSha,
      settingsSha256: "a".repeat(64),
      registrySha256: "b".repeat(64),
      canarySha256: "c".repeat(64),
      contentRetained: false,
      results: [
        {
          endpoint,
          classification: "passed",
          failureClass: null,
          receiptSha256: sha256(receiptPath),
        },
      ],
      passedCount: 1,
      externalUnavailableCount: 0,
      interactionRequiredCount: 0,
      productFailureCount: 0,
    };
    mutateSummary?.(surface, summary);
    const summaryPath = path.join(root, `${surface}-summary.json`);
    writeFileSync(summaryPath, `${JSON.stringify(summary)}\n`);
    surfaces[surface] = { receipts, summary: summaryPath };
  }
  mutateFixture?.(root, surfaces);
  const output = path.join(root, "aggregate.json");
  const argv = [
    aggregate,
    "--inventory", inventory,
    "--source-manifest", sourceManifest,
    "--tested-revision", "1".repeat(40),
    "--tested-tree", "2".repeat(40),
    "--attempt", "1",
    "--mac-summary", surfaces["mac-local"].summary,
    "--mac-receipts", surfaces["mac-local"].receipts,
    "--intrepid-summary", surfaces.intrepid.summary,
    "--intrepid-receipts", surfaces.intrepid.receipts,
    "--output", output,
  ];
  const process = spawnSync("/usr/bin/python3", argv, { encoding: "utf8" });
  return { process, output, argv };
}

test("aggregate binds both surfaces to exact route receipts", () => {
  const result = fixture();
  assert.equal(result.process.status, 0, result.process.stderr);
  const payload = JSON.parse(readFileSync(result.output, "utf8"));
  assert.equal(payload.schema, "zed-agent-matrix-v2");
  assert.deepEqual(payload.surfaces.map((surface) => surface.expectedCount), [1, 1]);
  assert.equal(payload.permissionsApproved, 0);
  assert.equal(payload.allDirectProcessGroupsGone, true);
});

test("aggregate rejects classifications the runner cannot emit", () => {
  const result = fixture({
    mutateSummary: (_surface, summary) => {
      summary.results[0].classification = "passed_installed_ui";
    },
  });
  assert.equal(result.process.status, 1);
  assert.match(result.process.stderr, /invalid_route_classification/);
});

test("aggregate rejects timeout as external unavailability", () => {
  const result = fixture({
    mutateReceipt: (_surface, receipt) => {
      receipt.status = "failed";
      receipt.failureClass = "timeout";
    },
    mutateSummary: (_surface, summary) => {
      summary.results[0].classification = "external_unavailable";
      summary.results[0].failureClass = "timeout";
      summary.passedCount = 0;
      summary.externalUnavailableCount = 1;
    },
  });
  assert.equal(result.process.status, 1);
  assert.match(result.process.stderr, /classification_failure_mismatch/);
});

test("aggregate rejects a missing or altered route receipt", () => {
  const result = fixture({
    mutateSummary: (_surface, summary) => {
      summary.results[0].receiptSha256 = "0".repeat(64);
    },
  });
  assert.equal(result.process.status, 1);
  assert.match(result.process.stderr, /receipt_result_mismatch/);
});

test("aggregate rejects a receipt reused with contradictory route identity", () => {
  const result = fixture({
    mutateFixture: (_root, surfaces) => {
      const macReceipt = path.join(surfaces["mac-local"].receipts, "01-Mac-Route.json");
      const intrepidReceipt = path.join(
        surfaces.intrepid.receipts,
        "01-Intrepid-Route.json",
      );
      writeFileSync(intrepidReceipt, readFileSync(macReceipt));
      const summary = JSON.parse(readFileSync(surfaces.intrepid.summary, "utf8"));
      summary.results[0].receiptSha256 = sha256(intrepidReceipt);
      writeFileSync(surfaces.intrepid.summary, `${JSON.stringify(summary)}\n`);
    },
  });
  assert.equal(result.process.status, 1);
  assert.match(result.process.stderr, /reused_route_receipt/);
});

test("aggregate output is immutable and cannot be replayed", () => {
  const result = fixture();
  assert.equal(result.process.status, 0, result.process.stderr);
  const replay = spawnSync("/usr/bin/python3", result.argv, { encoding: "utf8" });
  assert.notEqual(replay.status, 0);
});

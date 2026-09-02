import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const repositoryRoot = path.resolve(import.meta.dirname, "../..");
const matrix = path.join(repositoryRoot, "script/zed-agent-picker-uat.py");
const fakeCanary = path.join(
  repositoryRoot,
  "script/tests/fixtures/fake-zed-picker-canary.py",
);

function runMatrix({
  expected,
  configured = expected,
  existingSummary = false,
  driftSource = false,
  registryEntries = [],
  sourceExclusions = { Darwin: [], Linux: [] },
  sourceHostExclusions = { Darwin: [], Linux: [] },
  catalogHostExclusions = sourceHostExclusions,
  inventoryExclusions = { Darwin: [], Linux: [] },
  sentinel = "sentinel.txt",
  ephemeral = false,
}) {
  const root = mkdtempSync(path.join(tmpdir(), "zed-picker-matrix-"));
  const project = path.join(root, "project");
  const outputDir = path.join(root, "receipts");
  const registry = path.join(root, "registry");
  mkdirSync(project);
  mkdirSync(outputDir, { mode: 0o700 });
  chmodSync(outputDir, 0o700);
  mkdirSync(registry);
  if (!ephemeral) {
    writeFileSync(path.join(project, "sentinel.txt"), "picker-matrix\n");
  }
  writeFileSync(path.join(registry, "registry.json"), JSON.stringify({ agents: [] }));
  const macCustom = expected.filter((name) => !registryEntries.includes(name));
  const macRegistry = registryEntries.filter(
    (name) => !inventoryExclusions.Darwin.includes(name),
  );
  const intrepidRegistry = registryEntries.filter(
    (name) => !inventoryExclusions.Linux.includes(name),
  );
  const linuxLocal = ["Other Surface"];
  const sourceMac = [
    ...new Set([...macCustom, ...(catalogHostExclusions?.Darwin ?? [])]),
  ];
  const sourcePersistent = Object.fromEntries(
    (catalogHostExclusions?.Linux ?? []).map((name, index) => [
      name,
      `excluded-${index}`,
    ]),
  );
  const managed = [
    ...new Set([
      ...expected,
      ...configured,
      ...registryEntries,
      ...sourceMac,
      ...Object.keys(sourcePersistent),
      "Other Surface",
    ]),
  ];
  const inventory = path.join(root, "inventory.json");
  writeFileSync(
    inventory,
    JSON.stringify({
      schema: "zed-agent-picker-inventory-v1",
      managedEntries: managed,
      surfaces: {
        "mac-local": expected,
        intrepid: [...linuxLocal, ...intrepidRegistry],
      },
      executionClasses: {
        "mac-custom": {
          surface: "mac-local",
          representative: macCustom[0] ?? null,
          members: macCustom,
        },
        "mac-registry": {
          surface: "mac-local",
          representative: macRegistry[0] ?? null,
          members: macRegistry,
        },
        "intrepid-local": {
          surface: "intrepid",
          representative: linuxLocal[0],
          members: linuxLocal,
        },
        "intrepid-persistent": { surface: "intrepid", representative: null, members: [] },
        "intrepid-registry": {
          surface: "intrepid",
          representative: intrepidRegistry[0] ?? null,
          members: intrepidRegistry,
        },
      },
    }),
  );
  const sourceManifest = path.join(root, "agent-servers.json");
  const sourceManaged = driftSource ? [...managed, "Source Drift"] : managed;
  const sourceLinuxLocal = driftSource
    ? [...linuxLocal, "Source Drift"]
    : linuxLocal;
  writeFileSync(
    sourceManifest,
    JSON.stringify({
      schemaVersion: 2,
      managedNames: sourceManaged,
      macLanes: sourceMac,
      linuxLocalLanes: sourceLinuxLocal,
      persistentLanes: sourcePersistent,
      projectHostRegistryLanes: registryEntries,
      projectHostRegistryExclusions: sourceExclusions,
      hostRouteExclusions: sourceHostExclusions,
      agentServers: Object.fromEntries(
        sourceManaged.map((name) => [
          name,
          registryEntries.includes(name) ? { type: "registry" } : {},
        ]),
      ),
    }),
  );
  const settings = path.join(root, "settings.json");
  writeFileSync(
    settings,
    JSON.stringify({
      agent_servers: Object.fromEntries(
        configured.map((name) => [name, { type: "custom", command: "/bin/true", args: [] }]),
      ),
    }),
  );
  const summary = path.join(root, "summary.json");
  if (existingSummary) writeFileSync(summary, "{}\n");
  const log = path.join(root, "calls.log");
  const processResult = spawnSync(
    "/usr/bin/python3",
    [
      matrix,
      "--surface",
      "mac-local",
      "--inventory",
      inventory,
      "--source-manifest",
      sourceManifest,
      "--settings",
      settings,
      "--registry-cache",
      registry,
      "--npm-command",
      "/usr/bin/python3",
      "--cwd",
      project,
      "--sentinel",
      sentinel,
      ...(ephemeral ? ["--ephemeral-sentinel"] : []),
      "--output-dir",
      outputDir,
      "--summary",
      summary,
      "--canary",
      fakeCanary,
      "--timeout-seconds",
      "5",
      "--termination-grace-seconds",
      "0.1",
    ],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: { ...process.env, FAKE_CANARY_LOG: log },
      timeout: 15_000,
    },
  );
  return {
    process: processResult,
    calls: existsSync(log)
      ? readFileSync(log, "utf8").trim().split("\n").filter(Boolean)
      : [],
    summary: existingSummary ? null : JSON.parse(readFileSync(summary, "utf8")),
  };
}

test("picker matrix invokes every advertised entry exactly once in declared order", () => {
  const result = runMatrix({ expected: ["Alpha", "Beta", "Gamma"] });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Alpha", "Beta", "Gamma"]);
  assert.equal(result.summary.status, "pass");
  assert.equal(result.summary.passedCount, 3);
  assert.equal(result.summary.productFailureCount, 0);
});

test("explicit external readiness failures degrade without hiding complete coverage", () => {
  const result = runMatrix({ expected: ["Alpha", "Auth Route"] });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Alpha", "Auth Route"]);
  assert.equal(result.summary.status, "pass");
  assert.equal(result.summary.externalUnavailableCount, 1);
  assert.equal(result.summary.results[1].classification, "external_unavailable");
});

test("an unapproved interactive permission prompt is explicit rather than a product failure", () => {
  const result = runMatrix({ expected: ["Alpha", "Permission Route"] });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Alpha", "Permission Route"]);
  assert.equal(result.summary.status, "pass");
  assert.equal(result.summary.interactionRequiredCount, 1);
  assert.equal(result.summary.productFailureCount, 0);
  assert.equal(result.summary.results[1].classification, "interaction_required");
});

test("a product-origin route failure blocks the matrix", () => {
  const result = runMatrix({ expected: ["Alpha", "Product Route"] });
  assert.equal(result.process.status, 1);
  assert.deepEqual(result.calls, ["Alpha", "Product Route"]);
  assert.equal(result.summary.failureClass, "picker_product_failure");
  assert.equal(result.summary.productFailureCount, 1);
});

test("unsupported client behavior is not reclassified as external unavailability", () => {
  const result = runMatrix({ expected: ["Alpha", "Unsupported Route"] });
  assert.equal(result.process.status, 1);
  assert.deepEqual(result.calls, ["Alpha", "Unsupported Route"]);
  assert.equal(result.summary.failureClass, "picker_product_failure");
  assert.equal(result.summary.productFailureCount, 1);
});

test("an invalid sentinel fails once before any route starts", () => {
  const result = runMatrix({
    expected: ["Alpha", "Beta"],
    sentinel: "/etc/hosts",
  });
  assert.equal(result.process.status, 1);
  assert.deepEqual(result.calls, []);
  assert.equal(result.summary.failureClass, "invalid_sentinel");
  assert.equal(result.summary.productFailureCount, 0);
});

test("picker matrix permits one canary-owned ephemeral sentinel per route", () => {
  const result = runMatrix({
    expected: ["Alpha", "Beta"],
    ephemeral: true,
  });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Alpha", "Beta"]);
  assert.equal(result.summary.status, "pass");
  assert.equal(result.summary.ephemeralSentinel, true);
  assert.equal(result.summary.passedCount, 2);
});

test("omitted or stale managed picker entries fail before any route starts", () => {
  const result = runMatrix({ expected: ["Alpha", "Beta"], configured: ["Alpha"] });
  assert.equal(result.process.status, 1);
  assert.deepEqual(result.calls, []);
  assert.equal(result.summary.failureClass, "picker_inventory_mismatch");
});

test("source manifest drift fails before any route starts", () => {
  const result = runMatrix({ expected: ["Alpha", "Beta"], driftSource: true });
  assert.equal(result.process.status, 1);
  assert.deepEqual(result.calls, []);
  assert.equal(result.summary.failureClass, "source_inventory_mismatch");
});

test("source manifest host exclusions drive the exact picker inventory", () => {
  const result = runMatrix({
    expected: ["Mac Route", "Registry Kept"],
    registryEntries: ["Registry Kept", "Registry Excluded"],
    sourceExclusions: { Darwin: ["Registry Excluded"], Linux: [] },
    inventoryExclusions: { Darwin: ["Registry Excluded"], Linux: [] },
  });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Mac Route", "Registry Kept"]);
  assert.equal(result.summary.expectedEndpoints.length, 2);
});

test("source manifest custom-route exclusions drive the exact picker inventory", () => {
  const result = runMatrix({
    expected: ["Mac Route"],
    sourceHostExclusions: {
      Darwin: ["Mac Route Excluded"],
      Linux: ["Intrepid Route Excluded"],
    },
  });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Mac Route"]);
  assert.deepEqual(result.summary.expectedEndpoints, ["Mac Route"]);
});

test("source manifest permits one registry route to be excluded on both hosts", () => {
  const result = runMatrix({
    expected: ["Mac Route"],
    registryEntries: ["Registry Route"],
    sourceExclusions: {
      Darwin: ["Registry Route"],
      Linux: ["Registry Route"],
    },
    inventoryExclusions: {
      Darwin: ["Registry Route"],
      Linux: ["Registry Route"],
    },
  });
  assert.equal(result.process.status, 0, result.process.stderr);
  assert.deepEqual(result.calls, ["Mac Route"]);
});

test("source manifest rejects missing or malformed registry exclusions", () => {
  for (const sourceExclusions of [
    null,
    { Darwin: ["Registry Route"] },
    { Darwin: ["Registry Route", "Registry Route"], Linux: [] },
  ]) {
    const result = runMatrix({
      expected: ["Registry Route"],
      registryEntries: ["Registry Route"],
      sourceExclusions,
    });
    assert.equal(result.process.status, 1);
    assert.deepEqual(result.calls, []);
    assert.equal(result.summary.failureClass, "invalid_source_manifest");
  }
});

test("source manifest rejects missing or cross-host custom-route exclusions", () => {
  for (const sourceHostExclusions of [
    null,
    { Darwin: ["Mac Route"] },
    { Darwin: ["Intrepid Route"], Linux: [] },
    { Darwin: [], Linux: ["Mac Route"] },
  ]) {
    const result = runMatrix({
      expected: ["Mac Route"],
      sourceHostExclusions,
      catalogHostExclusions: {
        Darwin: ["Mac Route"],
        Linux: ["Intrepid Route"],
      },
    });
    assert.equal(result.process.status, 1);
    assert.deepEqual(result.calls, []);
    assert.equal(result.summary.failureClass, "invalid_source_manifest");
  }
});

test("an immutable existing summary prevents replay", () => {
  const result = runMatrix({ expected: ["Alpha"], existingSummary: true });
  assert.equal(result.process.status, 2);
  assert.deepEqual(result.calls, []);
});

test("checked inventory binds every configured route and all 22 visible picker variants", () => {
  const inventory = JSON.parse(
    readFileSync(
      path.join(repositoryRoot, "docs/test-plan-inputs/zed-agent-picker-inventory.json"),
      "utf8",
    ),
  );
  assert.equal(inventory.managedEntries.length, 23);
  assert.equal(inventory.surfaces["mac-local"].length, 8);
  assert.equal(inventory.surfaces.intrepid.length, 16);
  assert.ok(inventory.surfaces.intrepid.includes("Codex (Intrepid, work)"));
  assert.equal(new Set(inventory.managedEntries).size, 23);
  assert.deepEqual(Object.keys(inventory.executionClasses), [
    "mac-custom",
    "mac-registry",
    "intrepid-local",
    "intrepid-persistent",
    "intrepid-registry",
  ]);
  for (const executionClass of Object.values(inventory.executionClasses)) {
    assert.ok(executionClass.members.includes(executionClass.representative));
  }
  assert.deepEqual(
    Object.values(inventory.executionClasses)
      .filter((executionClass) => executionClass.surface === "mac-local")
      .flatMap((executionClass) => executionClass.members),
    inventory.surfaces["mac-local"],
  );
  assert.deepEqual(
    Object.values(inventory.executionClasses)
      .filter((executionClass) => executionClass.surface === "intrepid")
      .flatMap((executionClass) => executionClass.members),
    inventory.surfaces.intrepid,
  );
  assert.deepEqual(
    inventory.surfaces["mac-local"].filter((name) =>
      inventory.surfaces.intrepid.includes(name),
    ),
    ["cursor", "grok-build"],
  );

  assert.deepEqual(Object.keys(inventory.pickerSurfaces), [
    "mac-local",
    "intrepid-persistent",
    "intrepid-ordinary",
  ]);
  assert.equal(inventory.pickerSurfaces["mac-local"].length, 8);
  assert.equal(inventory.pickerSurfaces["intrepid-persistent"].length, 8);
  assert.equal(inventory.pickerSurfaces["intrepid-ordinary"].length, 6);

  const pickerVariants = Object.values(inventory.pickerSurfaces).flat();
  assert.equal(pickerVariants.length, 22);
  assert.equal(new Set(pickerVariants.map((variant) => variant.id)).size, 22);
  assert.equal(new Set(pickerVariants.map((variant) => variant.label)).size, 22);
  for (const variant of pickerVariants) {
    assert.match(variant.id, /^VAR-/);
    assert.ok(variant.label.includes("(Mac") || variant.label.includes("(Intrepid"));
    assert.ok(variant.configuredEntries.length >= 1);
  }

  assert.deepEqual(
    new Set(pickerVariants.flatMap((variant) => variant.configuredEntries)),
    new Set([
      ...inventory.surfaces["mac-local"],
      ...inventory.surfaces.intrepid,
    ]),
    "every configured route must map to one visible picker variant",
  );
  assert.deepEqual(
    pickerVariants
      .filter((variant) => variant.configuredEntries.length > 1)
      .map((variant) => ({
        label: variant.label,
        configuredEntries: variant.configuredEntries,
      })),
    [
      {
        label: "GLM (Intrepid)",
        configuredEntries: ["GLM (Intrepid)", "glm-acp-agent"],
      },
      {
        label: "Kimi (Intrepid)",
        configuredEntries: ["Kimi (Intrepid)", "kimi"],
      },
    ],
    "only the two deliberate custom-over-registry aliases may collapse",
  );
});

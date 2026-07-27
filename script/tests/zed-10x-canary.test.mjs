import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import test from "node:test";

import {
  appendEvent,
  buildComparison,
  createTraceContext,
  discoverProcessIds,
  hashProjectIdentifier,
  isDisabled,
  pruneStore,
  sensitiveEnvironmentNames,
} from "../zed-10x-canary.mjs";

function temporaryStore() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-canary-"));
}

async function waitFor(predicate, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return predicate();
}

function event(overrides = {}) {
  return {
    name: "app.launch",
    cohort: "zed10x",
    lane: "local",
    appVersion: "1.13.0-10x",
    buildVersion: "20260724.1",
    gitCommit: "4516ad1f760ab41a1f8d3c00a1d5cc005c0c3a5b",
    attributes: {},
    timestamp: "2026-07-24T12:00:00.000Z",
    ...overrides,
  };
}

function comparisonGroup({
  cohort,
  lane = "local",
  buildVersion,
  sessions = 5,
  failureEvents = 0,
  startMs = Date.parse("2026-07-24T12:00:00.000Z"),
  durationMs = 30 * 60 * 1000,
}) {
  return [
    ...Array.from({ length: sessions }, (_, index) =>
      event({
        cohort,
        lane,
        buildVersion,
        name: "app.launch",
        timestamp: new Date(startMs + index * 1000).toISOString(),
      }),
    ),
    ...Array.from({ length: failureEvents }, (_, index) =>
      event({
        cohort,
        lane,
        buildVersion,
        name: "acp.disconnect",
        timestamp: new Date(startMs + 60_000 + index * 1000).toISOString(),
      }),
    ),
    event({
      cohort,
      lane,
      buildVersion,
      name: "resource.sample",
      timestamp: new Date(startMs + durationMs).toISOString(),
    }),
  ];
}

test("disabled telemetry writes nothing", () => {
  const store = temporaryStore();
  assert.equal(isDisabled({ ZED_10X_TELEMETRY_DISABLED: "1" }, store), true);
  assert.equal(appendEvent(event(), { storeDir: store, disabled: true }), false);
  assert.deepEqual(fs.readdirSync(store), []);
});

test("a DISABLED sentinel provides a reversible local kill switch", () => {
  const store = temporaryStore();
  fs.writeFileSync(path.join(store, "DISABLED"), "disabled by operator\n");
  assert.equal(isDisabled({}, store), true);
});

test("sensitive and unrecognised attributes are rejected", () => {
  const store = temporaryStore();
  assert.equal(
    appendEvent(event({ attributes: { prompt: "never store me" } }), {
      storeDir: store,
    }),
    false,
  );
  assert.equal(
    appendEvent(event({ attributes: { "process.command_args": ["--token", "secret"] } }), {
      storeDir: store,
    }),
    false,
  );
  assert.deepEqual(fs.readdirSync(store), []);
});

test("valid events are private, content-free JSONL records", () => {
  const store = temporaryStore();
  const trace = createTraceContext();
  assert.equal(
    appendEvent(
      event({
        traceparent: trace.traceparent,
        attributes: {
          "project.id": hashProjectIdentifier("/Users/example/secret-project"),
          "duration.ms": 712,
          "process.rss_bytes": 1024,
        },
      }),
      { storeDir: store },
    ),
    true,
  );

  const eventFile = fs
    .readdirSync(store)
    .find((file) => file.startsWith("events-") && file.endsWith(".jsonl"));
  assert.ok(eventFile);
  const eventPath = path.join(store, eventFile);
  const mode = fs.statSync(eventPath).mode & 0o777;
  assert.equal(mode, 0o600);
  const record = JSON.parse(fs.readFileSync(eventPath, "utf8").trim());
  assert.equal(record.body, "app.launch");
  assert.equal(record.trace_id.length, 32);
  assert.equal(record.span_id.length, 16);
  assert.equal(record.attributes["zed.cohort"], "zed10x");
  assert.equal(record.attributes["project.id"].length, 64);
  assert.equal(JSON.stringify(record).includes("secret-project"), false);
});

test("telemetry storage failures are fail-open", () => {
  const parent = temporaryStore();
  const blocker = path.join(parent, "not-a-directory");
  fs.writeFileSync(blocker, "block");
  assert.doesNotThrow(() => {
    assert.equal(appendEvent(event(), { storeDir: blocker }), false);
  });
});

test("retention removes expired and excess event files", () => {
  const store = temporaryStore();
  const old = path.join(store, "events-2026-06-01.jsonl");
  const current = path.join(store, "events-2026-07-24.jsonl");
  fs.writeFileSync(old, "old\n", { mode: 0o600 });
  fs.writeFileSync(current, "x".repeat(2048), { mode: 0o600 });
  fs.utimesSync(old, new Date("2026-06-01T00:00:00Z"), new Date("2026-06-01T00:00:00Z"));

  pruneStore(store, {
    now: new Date("2026-07-24T12:00:00Z"),
    retentionDays: 14,
    maxBytes: 1024,
  });

  assert.equal(fs.existsSync(old), false);
  assert.equal(fs.statSync(current).size <= 1024, true);
});

test("retention removes expired incidents and keeps bounded complete records", () => {
  const store = temporaryStore();
  const incidentsPath = path.join(store, "incidents.jsonl");
  const incident = (incidentId, openedAt) =>
    JSON.stringify({
      schema_version: 1,
      incident_id: incidentId,
      opened_at: openedAt,
      failure_class: "repeated_acp_disconnect",
      cohort_lane_build: "zed10x:local:20260724.1",
    });
  fs.writeFileSync(
    incidentsPath,
    [
      incident("expired", "2026-06-01T00:00:00.000Z"),
      ...Array.from({ length: 8 }, (_, index) =>
        incident(`current-${index}`, `2026-07-24T12:0${index}:00.000Z`),
      ),
      "",
    ].join("\n"),
    { mode: 0o600 },
  );

  pruneStore(store, {
    now: new Date("2026-07-24T13:00:00Z"),
    retentionDays: 14,
    maxBytes: 512,
  });

  const retained = fs
    .readFileSync(incidentsPath, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  assert.equal(fs.statSync(incidentsPath).size <= 512, true);
  assert.equal(retained.some((record) => record.incident_id === "expired"), false);
  assert.equal(retained.at(-1).incident_id, "current-7");
  assert.equal(fs.statSync(incidentsPath).mode & 0o777, 0o600);
});

test("recurring event appends throttle full-store retention sweeps", () => {
  const store = temporaryStore();
  const expired = path.join(store, "events-2026-06-01.jsonl");
  const oldTime = new Date("2026-06-01T00:00:00Z");
  const options = {
    storeDir: store,
    now: new Date("2026-07-24T13:00:00Z"),
    retentionDays: 14,
    pruneIntervalMs: 60_000,
  };
  fs.writeFileSync(expired, "old\n", { mode: 0o600 });
  fs.utimesSync(expired, oldTime, oldTime);

  assert.equal(appendEvent(event(), options), true);
  assert.equal(fs.existsSync(expired), false);

  fs.writeFileSync(expired, "old again\n", { mode: 0o600 });
  fs.utimesSync(expired, oldTime, oldTime);
  assert.equal(appendEvent(event({ name: "resource.sample" }), options), true);
  assert.equal(fs.existsSync(expired), true);
});

test("repeated disconnects create one durable linked incident candidate", () => {
  const store = temporaryStore();
  for (let index = 0; index < 3; index += 1) {
    assert.equal(
      appendEvent(
        event({
          name: "acp.disconnect",
          timestamp: `2026-07-24T12:0${index}:00.000Z`,
          attributes: { "acp.provider": "apex", "failure.class": "transport" },
        }),
        { storeDir: store },
      ),
      true,
    );
  }

  const incidents = fs
    .readFileSync(path.join(store, "incidents.jsonl"), "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  assert.equal(incidents.length, 1);
  assert.equal(incidents[0].tracking_issue, "https://github.com/keevaspeyer10x/zed-10x/issues/15");
  assert.equal(incidents[0].failure_class, "repeated_acp_disconnect");
  assert.equal("prompt" in incidents[0], false);
});

test("incident thresholds read only the current and previous UTC day", () => {
  const store = temporaryStore();
  const unrelated = path.join(store, "events-2026-07-01.jsonl");
  fs.writeFileSync(unrelated, "{}\n", { mode: 0o600 });
  fs.chmodSync(unrelated, 0o000);

  try {
    for (let index = 0; index < 3; index += 1) {
      assert.equal(
        appendEvent(
          event({
            name: "acp.disconnect",
            timestamp: `2026-07-24T12:0${index}:00.000Z`,
            attributes: { "acp.provider": "apex", "failure.class": "transport" },
          }),
          { storeDir: store, now: new Date("2026-07-24T12:05:00.000Z") },
        ),
        true,
      );
    }
    assert.equal(fs.existsSync(path.join(store, "incidents.jsonl")), true);
  } finally {
    fs.chmodSync(unrelated, 0o600);
  }
});

test("incident thresholds retain the previous day across midnight", () => {
  const store = temporaryStore();
  for (const timestamp of [
    "2026-07-23T23:50:00.000Z",
    "2026-07-23T23:55:00.000Z",
    "2026-07-24T00:05:00.000Z",
  ]) {
    assert.equal(
      appendEvent(
        event({
          name: "acp.disconnect",
          timestamp,
          attributes: { "acp.provider": "apex", "failure.class": "transport" },
        }),
        { storeDir: store, now: new Date("2026-07-24T00:05:00.000Z") },
      ),
      true,
    );
  }
  assert.equal(fs.existsSync(path.join(store, "incidents.jsonl")), true);
});

test("control incidents use the control cohort prefix", () => {
  const store = temporaryStore();
  for (let index = 0; index < 3; index += 1) {
    assert.equal(
      appendEvent(
        event({
          name: "acp.disconnect",
          cohort: "zed",
          buildVersion: "1.11.3",
          timestamp: `2026-07-24T12:0${index}:00.000Z`,
          attributes: { "acp.provider": "apex", "failure.class": "transport" },
        }),
        { storeDir: store },
      ),
      true,
    );
  }
  const [incident] = fs
    .readFileSync(path.join(store, "incidents.jsonl"), "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  assert.match(incident.incident_id, /^zed-/);
  assert.equal(incident.incident_id.startsWith("zed10x-"), false);
});

test("comparison is explicit about low confidence and separates cohort, lane, and build", () => {
  const records = [
    event({ name: "app.launch" }),
    event({ name: "acp.complete", attributes: { "duration.ms": 1000 } }),
    event({ name: "app.launch", cohort: "zed", buildVersion: "1.11.3" }),
    event({ name: "acp.disconnect", cohort: "zed", buildVersion: "1.11.3" }),
  ];
  const comparison = buildComparison(records);
  assert.equal(comparison.verdict, "insufficient_evidence");
  assert.equal(comparison.confidence, "low");
  assert.equal(comparison.groups.length, 2);
  assert.deepEqual(
    comparison.groups.map((group) => [group.cohort, group.lane, group.build_version]),
    [
      ["zed", "local", "1.11.3"],
      ["zed10x", "local", "20260724.1"],
    ],
  );
});

test("comparison only judges matched lanes and ignores sparse unrelated groups", () => {
  const credibleGroup = (cohort, lane, buildVersion, failureName) => [
    ...Array.from({ length: 5 }, (_, index) =>
      event({
        cohort,
        lane,
        buildVersion,
        name: "app.launch",
        timestamp: `2026-07-24T12:0${index}:00.000Z`,
      }),
    ),
    event({
      cohort,
      lane,
      buildVersion,
      name: failureName,
      timestamp: "2026-07-24T12:15:00.000Z",
    }),
    event({
      cohort,
      lane,
      buildVersion,
      name: "resource.sample",
      timestamp: "2026-07-24T12:31:00.000Z",
    }),
  ];
  const records = [
    ...credibleGroup("zed", "local", "1.11.3", "acp.disconnect"),
    ...credibleGroup("zed10x", "local", "20260727.1", "acp.complete"),
    event({
      cohort: "zed10x",
      lane: "intrepid",
      buildVersion: "20260727.1",
      name: "app.launch",
    }),
  ];

  const comparison = buildComparison(records);
  assert.equal(comparison.verdict, "zed10x_materially_more_reliable");
  assert.equal(comparison.confidence, "moderate");
  assert.deepEqual(comparison.comparisons, [
    {
      lane: "local",
      control_build_version: "1.11.3",
      canary_build_version: "20260727.1",
      control_sessions: 5,
      canary_sessions: 5,
      control_failure_events: 1,
      canary_failure_events: 0,
      control_failure_rate_per_launch: 0.2,
      canary_failure_rate_per_launch: 0,
      confidence: "moderate",
      verdict: "zed10x_materially_more_reliable",
    },
  ]);
});

test("credible comparisons handle equal and zero failure rates honestly", () => {
  const comparisonRecords = (controlFailures, canaryFailures) => {
    const records = [];
    for (const [cohort, buildVersion, failures] of [
      ["zed", "1.11.3", controlFailures],
      ["zed10x", "20260724.1", canaryFailures],
    ]) {
      for (let index = 0; index < 5; index += 1) {
        records.push(
          event({
            cohort,
            buildVersion,
            timestamp: new Date(Date.UTC(2026, 6, 24, 12, index * 8)).toISOString(),
          }),
        );
      }
      for (let index = 0; index < failures; index += 1) {
        records.push(
          event({
            name: "acp.disconnect",
            cohort,
            buildVersion,
            timestamp: new Date(Date.UTC(2026, 6, 24, 12, 10 + index)).toISOString(),
          }),
        );
      }
    }
    return records;
  };

  assert.equal(buildComparison(comparisonRecords(0, 0)).verdict, "no_material_difference");
  assert.equal(
    buildComparison(comparisonRecords(2, 0)).verdict,
    "zed10x_materially_more_reliable",
  );
  assert.equal(
    buildComparison(comparisonRecords(0, 2)).verdict,
    "zed10x_materially_less_reliable",
  );
  assert.equal(buildComparison(comparisonRecords(1, 1)).verdict, "no_material_difference");
});

test("comparison credibility uses the unrounded 30-minute boundary", () => {
  const shortComparison = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      buildVersion: "1.11.3",
      durationMs: 30 * 60 * 1000 - 1,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      buildVersion: "20260727.1",
    }),
  ]);
  assert.equal(shortComparison.groups[0].observed_minutes, 30);
  assert.equal(shortComparison.comparisons[0].confidence, "low");
  assert.equal(shortComparison.verdict, "insufficient_evidence");

  const exactComparison = buildComparison([
    ...comparisonGroup({ cohort: "zed", buildVersion: "1.11.3" }),
    ...comparisonGroup({ cohort: "zed10x", buildVersion: "20260727.1" }),
  ]);
  assert.equal(exactComparison.comparisons[0].confidence, "moderate");
});

test("comparison selects only the most recently observed build in each known lane", () => {
  const early = Date.parse("2026-07-24T08:00:00.000Z");
  const late = Date.parse("2026-07-24T12:00:00.000Z");
  const comparison = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      buildVersion: "9.9.9",
      startMs: early,
      failureEvents: 2,
    }),
    ...comparisonGroup({
      cohort: "zed",
      buildVersion: "1.0.0",
      startMs: late,
      failureEvents: 1,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      buildVersion: "99999999.1",
      startMs: early,
      failureEvents: 2,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      buildVersion: "20260727.1",
      startMs: late,
    }),
  ]);

  assert.equal(comparison.comparisons.length, 1);
  assert.equal(comparison.comparisons[0].control_build_version, "1.0.0");
  assert.equal(comparison.comparisons[0].canary_build_version, "20260727.1");
  assert.equal(comparison.verdict, "zed10x_materially_more_reliable");
});

test("unknown lanes are reported as groups but never compared", () => {
  const comparison = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      lane: "unknown",
      buildVersion: "1.11.3",
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      lane: "unknown",
      buildVersion: "20260727.1",
    }),
  ]);

  assert.equal(comparison.groups.length, 2);
  assert.deepEqual(comparison.comparisons, []);
  assert.equal(comparison.verdict, "insufficient_evidence");
});

test("credible lane verdicts agree explicitly or produce mixed results", () => {
  const agreeing = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      lane: "local",
      buildVersion: "1.11.3",
      failureEvents: 2,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      lane: "local",
      buildVersion: "20260727.1",
    }),
    ...comparisonGroup({
      cohort: "zed",
      lane: "intrepid",
      buildVersion: "1.11.3",
      failureEvents: 2,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      lane: "intrepid",
      buildVersion: "20260727.1",
    }),
  ]);
  assert.equal(agreeing.comparisons.length, 2);
  assert.equal(agreeing.verdict, "zed10x_materially_more_reliable");

  const mixed = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      lane: "local",
      buildVersion: "1.11.3",
      failureEvents: 2,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      lane: "local",
      buildVersion: "20260727.1",
    }),
    ...comparisonGroup({
      cohort: "zed",
      lane: "intrepid",
      buildVersion: "1.11.3",
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      lane: "intrepid",
      buildVersion: "20260727.1",
      failureEvents: 2,
    }),
  ]);
  assert.equal(mixed.verdict, "mixed_results");
});

test("zero-launch low-confidence groups do not invent a failure rate", () => {
  const comparison = buildComparison([
    ...comparisonGroup({
      cohort: "zed",
      buildVersion: "1.11.3",
      sessions: 0,
      failureEvents: 1,
    }),
    ...comparisonGroup({
      cohort: "zed10x",
      buildVersion: "20260727.1",
      sessions: 0,
    }),
  ]);

  assert.equal(comparison.comparisons[0].control_failure_rate_per_launch, null);
  assert.equal(comparison.comparisons[0].canary_failure_rate_per_launch, null);
  assert.equal(comparison.comparisons[0].confidence, "low");
});

test("trace context uses valid W3C traceparent widths", () => {
  const trace = createTraceContext();
  assert.match(trace.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
  assert.match(trace.traceId, /^[0-9a-f]{32}$/);
  assert.match(trace.spanId, /^[0-9a-f]{16}$/);
});

test("control observation matches only the exact official executable path", () => {
  const table = [
    "  101 /Applications/Zed.app/Contents/MacOS/zed",
    "  102 /Applications/Zed 10x.app/Contents/Resources/zed-10x",
    "  103 /tmp/zed",
    "  104 /Applications/Zed.app/Contents/MacOS/zed-helper",
  ].join("\n");
  assert.deepEqual(discoverProcessIds(table, "/Applications/Zed.app/Contents/MacOS/zed"), [101]);
});

test(
  "macOS ps comm exposes the exact executable path without command arguments",
  { skip: process.platform !== "darwin" },
  () => {
    const processTable = execFileSync(
      "/bin/ps",
      ["-p", String(process.pid), "-o", "pid=,comm="],
      { encoding: "utf8" },
    );
    assert.deepEqual(discoverProcessIds(processTable, process.execPath), [process.pid]);
    assert.equal(processTable.includes(import.meta.filename), false);
  },
);

test("control LaunchAgent clears inherited GUI credentials before Node starts", () => {
  const plist = fs.readFileSync(
    path.resolve("script/com.keeva.zed-control-canary.plist"),
    "utf8",
  );
  const envIndex = plist.indexOf("<string>/usr/bin/env</string>");
  const clearIndex = plist.indexOf("<string>-i</string>");
  const nodeIndex = plist.indexOf("<string>/opt/homebrew/bin/node</string>");
  const observerIndex = plist.indexOf("<string>observe-control</string>");
  const storeArgumentIndex = plist.indexOf("<string>--store</string>");
  assert.ok(envIndex >= 0 && clearIndex > envIndex && nodeIndex > clearIndex);
  assert.ok(observerIndex > nodeIndex && storeArgumentIndex > observerIndex);
  assert.equal(plist.includes("ZED_10X_CANARY_STORE="), false);
  assert.equal(plist.includes("API_KEY"), false);
  assert.equal(plist.includes("TOKEN="), false);
  assert.equal(plist.includes("SECRET="), false);
});

test("control observer fails closed when credential-shaped environment names arrive", () => {
  assert.deepEqual(
    sensitiveEnvironmentNames({
      HOME: "/Users/example",
      PATH: "/usr/bin:/bin",
      OLLAMA_CLOUD_API_KEY: "redacted",
      GITHUB_TOKEN: "redacted",
      ZED_10X_CANARY_STORE: "/tmp/canary",
    }),
    ["GITHUB_TOKEN", "OLLAMA_CLOUD_API_KEY"],
  );
  assert.deepEqual(
    sensitiveEnvironmentNames({ HOME: "/Users/example", PATH: "/usr/bin:/bin" }),
    [],
  );
});

test("launcher preserves app arguments while exposing only a project hash to telemetry", async () => {
  const root = temporaryStore();
  const appContents = path.join(root, "Zed 10x.app", "Contents");
  const macos = path.join(appContents, "MacOS");
  const resources = path.join(appContents, "Resources");
  const store = path.join(root, "canary-store");
  const fakeHome = path.join(root, "home");
  const userDataDir = path.join(
    fakeHome,
    "Library",
    "Application Support",
    "Zed 10x",
  );
  const resultPath = path.join(root, "fake-result.json");
  const collectorLog = path.join(root, "collector.log");
  const collectorEnvironmentPath = path.join(root, "collector-environment.txt");
  const fakeNodePath = path.join(root, "fake-node");
  fs.mkdirSync(fakeHome);
  fs.mkdirSync(macos, { recursive: true });
  fs.mkdirSync(resources, { recursive: true });
  fs.copyFileSync(
    path.resolve("script/zed-10x-canary-launcher"),
    path.join(macos, "zed-10x-launcher"),
  );
  fs.copyFileSync(
    path.resolve("script/zed-10x-canary.mjs"),
    path.join(resources, "zed-10x-canary.mjs"),
  );
  fs.chmodSync(path.join(macos, "zed-10x-launcher"), 0o755);
  fs.writeFileSync(
    path.join(macos, "zed-10x-runtime"),
    "#!/bin/bash\nnode -e 'require(\"node:fs\").writeFileSync(process.env.ZED_FAKE_RESULT, JSON.stringify({traceparent: process.env.TRACEPARENT, correlation: process.env.ZED_10X_CORRELATION_ID, cargoHome: process.env.CARGO_HOME, rustupHome: process.env.RUSTUP_HOME, path: process.env.PATH, args: process.argv.slice(1)}))' -- \"$@\"\nsleep 3\n",
    { mode: 0o755 },
  );
  fs.writeFileSync(
    path.join(appContents, "Info.plist"),
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleShortVersionString</key><string>1.13.0-10x</string><key>CFBundleVersion</key><string>20260724.1</string></dict></plist>",
  );
  fs.writeFileSync(
    path.join(resources, "zed-10x-git-commit"),
    "abcdef\n",
  );
  fs.writeFileSync(
    fakeNodePath,
    [
      "#!/bin/bash",
      `{ printf 'github=%s\\n' "\${GITHUB_TOKEN-unset}"; printf 'anthropic=%s\\n' "\${ANTHROPIC_API_KEY-unset}"; } > ${JSON.stringify(collectorEnvironmentPath)}`,
      `exec ${JSON.stringify(process.execPath)} "$@"`,
      "",
    ].join("\n"),
    { mode: 0o755 },
  );

  const projectPath = path.join(root, "private project");
  fs.mkdirSync(projectPath);
  const launcher = spawn(path.join(macos, "zed-10x-launcher"), [projectPath, "--wait"], {
    env: {
      ...process.env,
      ZED_10X_CANARY_NODE: fakeNodePath,
      ZED_10X_CANARY_STORE: store,
      ZED_10X_CANARY_DEBUG_LOG: collectorLog,
      ZED_FAKE_RESULT: resultPath,
      GITHUB_TOKEN: "collector-must-not-inherit",
      ANTHROPIC_API_KEY: "collector-must-not-inherit",
      HOME: fakeHome,
      CARGO_HOME: "/Users/example/Documents/build/cargo",
      RUSTUP_HOME: "/Users/example/Documents/build/rustup",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let standardError = "";
  launcher.stderr.setEncoding("utf8");
  launcher.stderr.on("data", (chunk) => {
    standardError += chunk;
  });
  const exitCode = await new Promise((resolve) => launcher.on("close", resolve));
  assert.equal(exitCode, 0, standardError);
  const collectorStarted = await waitFor(
    () =>
      fs.existsSync(store) &&
      fs.readdirSync(store).some((file) => file.startsWith("events-")),
  );

  const fakeResult = JSON.parse(fs.readFileSync(resultPath, "utf8"));
  assert.deepEqual(fakeResult.args, [
    "--user-data-dir",
    userDataDir,
    projectPath,
    "--wait",
  ]);
  assert.match(fakeResult.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
  assert.equal(fakeResult.correlation, fakeResult.traceparent.split("-")[1]);
  assert.equal(fakeResult.cargoHome, undefined);
  assert.equal(fakeResult.rustupHome, undefined);
  assert.equal(fakeResult.path.includes("/Documents/"), false);
  assert.deepEqual(fs.readFileSync(collectorEnvironmentPath, "utf8").trim().split("\n"), [
    "github=unset",
    "anthropic=unset",
  ]);

  assert.equal(
    collectorStarted,
    true,
    fs.existsSync(collectorLog) ? fs.readFileSync(collectorLog, "utf8") : "collector did not start",
  );
  const telemetry = fs
    .readdirSync(store)
    .filter((file) => file.startsWith("events-"))
    .flatMap((file) => fs.readFileSync(path.join(store, file), "utf8").trim().split("\n"))
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  assert.ok(telemetry.some((record) => record.body === "app.launch"));
  assert.equal(JSON.stringify(telemetry).includes(projectPath), false);
  assert.ok(
    telemetry.some(
      (record) => record.attributes["project.id"] === hashProjectIdentifier(projectPath),
    ),
  );
  assert.equal(
    telemetry.some((record) => "vcs.ref.head.revision" in record.attributes),
    false,
  );
});

test("record command never persists a raw remote hostname", () => {
  const store = temporaryStore();
  const privateHostname = "intrepid.private.example";
  const result = spawnSync(
    process.execPath,
    [
      path.resolve("script/zed-10x-canary.mjs"),
      "record",
      "remote.bootstrap.start",
      "--cohort",
      "zed10x",
      "--lane",
      "intrepid",
      "--remote-host",
      privateHostname,
      "--store",
      store,
    ],
    { encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr);

  const stored = fs
    .readdirSync(store)
    .filter((file) => file.startsWith("events-"))
    .map((file) => fs.readFileSync(path.join(store, file), "utf8"))
    .join("");
  const record = JSON.parse(stored.trim());
  assert.equal(stored.includes(privateHostname), false);
  assert.equal("remote.host" in record.attributes, false);
});

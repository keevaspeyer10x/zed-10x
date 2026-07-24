import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import test from "node:test";

import {
  appendEvent,
  buildComparison,
  createTraceContext,
  discoverProcessIds,
  hashProjectIdentifier,
  isDisabled,
  pruneStore,
} from "../zed-10x-canary.mjs";

function temporaryStore() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-canary-"));
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

test("control LaunchAgent clears inherited GUI credentials before Node starts", () => {
  const plist = fs.readFileSync(
    path.resolve("script/com.keeva.zed-control-canary.plist"),
    "utf8",
  );
  const envIndex = plist.indexOf("<string>/usr/bin/env</string>");
  const clearIndex = plist.indexOf("<string>-i</string>");
  const nodeIndex = plist.indexOf("<string>/opt/homebrew/bin/node</string>");
  assert.ok(envIndex >= 0 && clearIndex > envIndex && nodeIndex > clearIndex);
  assert.equal(plist.includes("API_KEY"), false);
  assert.equal(plist.includes("TOKEN="), false);
  assert.equal(plist.includes("SECRET="), false);
});

test("launcher preserves app arguments while exposing only a project hash to telemetry", async () => {
  const root = temporaryStore();
  const appContents = path.join(root, "Zed 10x.app", "Contents");
  const macos = path.join(appContents, "MacOS");
  const resources = path.join(appContents, "Resources");
  const store = path.join(root, "canary-store");
  const resultPath = path.join(root, "fake-result.json");
  const collectorLog = path.join(root, "collector.log");
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
    path.join(resources, "zed-10x"),
    "#!/bin/bash\nnode -e 'require(\"node:fs\").writeFileSync(process.env.ZED_FAKE_RESULT, JSON.stringify({traceparent: process.env.TRACEPARENT, correlation: process.env.ZED_10X_CORRELATION_ID, args: process.argv.slice(1)}))' -- \"$@\"\nsleep 3\n",
    { mode: 0o755 },
  );
  fs.writeFileSync(
    path.join(appContents, "Info.plist"),
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleShortVersionString</key><string>1.13.0-10x</string><key>CFBundleVersion</key><string>20260724.1</string></dict></plist>",
  );
  fs.writeFileSync(
    path.join(resources, "zed-10x-git-commit"),
    "4516ad1f760ab41a1f8d3c00a1d5cc005c0c3a5b\n",
  );

  const projectPath = path.join(root, "private project");
  fs.mkdirSync(projectPath);
  const launcher = spawn(path.join(macos, "zed-10x-launcher"), [projectPath, "--wait"], {
    env: {
      ...process.env,
      ZED_10X_CANARY_NODE: process.execPath,
      ZED_10X_CANARY_STORE: store,
      ZED_10X_CANARY_DEBUG_LOG: collectorLog,
      ZED_FAKE_RESULT: resultPath,
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
  await new Promise((resolve) => setTimeout(resolve, 750));

  const fakeResult = JSON.parse(fs.readFileSync(resultPath, "utf8"));
  assert.deepEqual(fakeResult.args, [projectPath, "--wait"]);
  assert.match(fakeResult.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
  assert.equal(fakeResult.correlation, fakeResult.traceparent.split("-")[1]);

  assert.equal(fs.existsSync(store), true, fs.existsSync(collectorLog) ? fs.readFileSync(collectorLog, "utf8") : "collector did not start");
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
});

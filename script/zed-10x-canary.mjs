#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const TRACKING_ISSUE = "https://github.com/keevaspeyer10x/zed-10x/issues/15";
const DEFAULT_STORE = path.join(
  os.homedir(),
  "Library",
  "Application Support",
  "Zed 10x",
  "dogfood-canary",
);
const DEFAULT_RETENTION_DAYS = 14;
const DEFAULT_MAX_BYTES = 8 * 1024 * 1024;
const TRACEPARENT_PATTERN = /^00-([0-9a-f]{32})-([0-9a-f]{16})-01$/;
const HASH_PATTERN = /^[0-9a-f]{64}$/;
const COMMIT_PATTERN = /^[0-9a-f]{7,64}$/;
const TOKEN_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;

const ALLOWED_EVENTS = new Set([
  "app.launch",
  "app.process_exit",
  "app.restart",
  "app.hang",
  "app.crash",
  "app.force_quit",
  "project.open.requested",
  "project.open.completed",
  "remote.bootstrap.start",
  "remote.bootstrap.complete",
  "remote.bootstrap.failure",
  "acp.start",
  "acp.complete",
  "acp.disconnect",
  "acp.reconnect",
  "session.continuity.check",
  "session.continuity.success",
  "session.continuity.failure",
  "resource.sample",
]);

const NUMERIC_ATTRIBUTES = new Set([
  "app.pid",
  "duration.ms",
  "process.cpu_percent",
  "process.elapsed_seconds",
  "process.rss_bytes",
]);

const BOOLEAN_ATTRIBUTES = new Set(["session.resumed", "telemetry.fail_open"]);

const TOKEN_ATTRIBUTES = new Set([
  "acp.provider",
  "continuity.result",
  "continuity.trigger",
  "exit.class",
  "failure.class",
  "process.state",
  "remote.host",
  "resource.pressure",
  "session.kind",
]);

const HASH_ATTRIBUTES = new Set(["project.id", "session.id_hash"]);

export function createTraceContext() {
  const traceId = nonZeroHex(16);
  const spanId = nonZeroHex(8);
  return {
    traceId,
    spanId,
    traceparent: `00-${traceId}-${spanId}-01`,
  };
}

function nonZeroHex(byteLength) {
  let value = "";
  while (!value || /^0+$/.test(value)) {
    value = crypto.randomBytes(byteLength).toString("hex");
  }
  return value;
}

export function hashProjectIdentifier(projectPath) {
  return crypto.createHash("sha256").update(String(projectPath)).digest("hex");
}

export function isDisabled(environment = process.env, storeDir = DEFAULT_STORE) {
  const flag = String(environment.ZED_10X_TELEMETRY_DISABLED ?? "").toLowerCase();
  return ["1", "true", "yes", "on"].includes(flag) || fs.existsSync(path.join(storeDir, "DISABLED"));
}

function validateText(value, pattern, field) {
  if (typeof value !== "string" || !pattern.test(value)) {
    throw new Error(`invalid ${field}`);
  }
  return value;
}

function validateAttributes(attributes) {
  if (attributes === undefined) return {};
  if (!attributes || typeof attributes !== "object" || Array.isArray(attributes)) {
    throw new Error("attributes must be an object");
  }

  const validated = {};
  for (const [key, value] of Object.entries(attributes)) {
    if (NUMERIC_ATTRIBUTES.has(key)) {
      if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
        throw new Error(`invalid numeric attribute ${key}`);
      }
      validated[key] = value;
    } else if (BOOLEAN_ATTRIBUTES.has(key)) {
      if (typeof value !== "boolean") throw new Error(`invalid boolean attribute ${key}`);
      validated[key] = value;
    } else if (TOKEN_ATTRIBUTES.has(key)) {
      validated[key] = validateText(value, TOKEN_PATTERN, key);
    } else if (HASH_ATTRIBUTES.has(key)) {
      validated[key] = validateText(value, HASH_PATTERN, key);
    } else {
      throw new Error(`attribute ${key} is not allow-listed`);
    }
  }
  return validated;
}

function normalizeEvent(input) {
  if (!ALLOWED_EVENTS.has(input.name)) throw new Error("event name is not allow-listed");
  if (!["zed", "zed10x"].includes(input.cohort)) throw new Error("invalid cohort");
  if (!["local", "intrepid", "unknown"].includes(input.lane)) throw new Error("invalid lane");

  const timestamp = new Date(input.timestamp ?? Date.now());
  if (Number.isNaN(timestamp.getTime())) throw new Error("invalid timestamp");

  const trace = input.traceparent
    ? parseTraceparent(input.traceparent)
    : createTraceContext();
  const appVersion = validateText(String(input.appVersion ?? "unknown"), /^[A-Za-z0-9][A-Za-z0-9.+_-]{0,63}$/, "appVersion");
  const buildVersion = validateText(String(input.buildVersion ?? "unknown"), /^[A-Za-z0-9][A-Za-z0-9.+_-]{0,63}$/, "buildVersion");
  const gitCommit = input.gitCommit
    ? validateText(input.gitCommit, COMMIT_PATTERN, "gitCommit")
    : undefined;

  return {
    time_unix_nano: String(BigInt(timestamp.getTime()) * 1_000_000n),
    observed_time_unix_nano: String(BigInt(Date.now()) * 1_000_000n),
    severity_text: input.name.includes("failure") || input.name.includes("disconnect") || input.name.includes("crash") || input.name.includes("hang")
      ? "WARN"
      : "INFO",
    body: input.name,
    trace_id: trace.traceId,
    span_id: trace.spanId,
    attributes: {
      "service.name": input.cohort === "zed10x" ? "zed-10x" : "zed",
      "service.version": appVersion,
      "zed.build_version": buildVersion,
      "zed.cohort": input.cohort,
      "zed.lane": input.lane,
      ...(gitCommit ? { "vcs.ref.head.revision": gitCommit } : {}),
      ...validateAttributes(input.attributes),
    },
  };
}

function parseTraceparent(traceparent) {
  const match = TRACEPARENT_PATTERN.exec(traceparent);
  if (!match || /^0+$/.test(match[1]) || /^0+$/.test(match[2])) {
    throw new Error("invalid traceparent");
  }
  return { traceId: match[1], spanId: match[2], traceparent };
}

function ensurePrivateDirectory(storeDir) {
  fs.mkdirSync(storeDir, { recursive: true, mode: 0o700 });
  fs.chmodSync(storeDir, 0o700);
}

function appendPrivateLine(filePath, value) {
  const descriptor = fs.openSync(filePath, "a", 0o600);
  try {
    fs.fchmodSync(descriptor, 0o600);
    fs.writeSync(descriptor, `${JSON.stringify(value)}\n`, null, "utf8");
  } finally {
    fs.closeSync(descriptor);
  }
}

export function appendEvent(input, options = {}) {
  const storeDir = options.storeDir ?? DEFAULT_STORE;
  if (options.disabled || isDisabled(options.environment, storeDir)) return false;

  try {
    const record = normalizeEvent(input);
    ensurePrivateDirectory(storeDir);
    pruneStore(storeDir, options);
    const day = new Date(Number(BigInt(record.time_unix_nano) / 1_000_000n))
      .toISOString()
      .slice(0, 10);
    appendPrivateLine(path.join(storeDir, `events-${day}.jsonl`), record);
    evaluateIncidentThresholds(storeDir, record);
    return true;
  } catch {
    return false;
  }
}

export function pruneStore(storeDir, options = {}) {
  try {
    if (!fs.existsSync(storeDir) || !fs.statSync(storeDir).isDirectory()) return;
    const now = options.now ?? new Date();
    const retentionDays = options.retentionDays ?? DEFAULT_RETENTION_DAYS;
    const maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES;
    const expiry = now.getTime() - retentionDays * 24 * 60 * 60 * 1000;
    const eventFiles = fs
      .readdirSync(storeDir)
      .filter((file) => /^events-\d{4}-\d{2}-\d{2}\.jsonl$/.test(file))
      .map((file) => path.join(storeDir, file));

    for (const filePath of eventFiles) {
      const statistics = fs.statSync(filePath);
      if (statistics.mtimeMs < expiry) {
        fs.unlinkSync(filePath);
        continue;
      }
      if (statistics.size > maxBytes) {
        const descriptor = fs.openSync(filePath, "r");
        try {
          const start = statistics.size - maxBytes;
          const buffer = Buffer.alloc(maxBytes);
          fs.readSync(descriptor, buffer, 0, maxBytes, start);
          const firstNewline = buffer.indexOf(0x0a);
          const retained = firstNewline >= 0 ? buffer.subarray(firstNewline + 1) : buffer;
          fs.writeFileSync(filePath, retained, { mode: 0o600 });
          fs.chmodSync(filePath, 0o600);
        } finally {
          fs.closeSync(descriptor);
        }
      }
    }
  } catch {
    // Retention is deliberately best-effort and may not affect editor startup.
  }
}

function loadEventRecords(storeDir) {
  try {
    return fs
      .readdirSync(storeDir)
      .filter((file) => /^events-\d{4}-\d{2}-\d{2}\.jsonl$/.test(file))
      .sort()
      .flatMap((file) =>
        fs
          .readFileSync(path.join(storeDir, file), "utf8")
          .split("\n")
          .filter(Boolean)
          .flatMap((line) => {
            try {
              return [JSON.parse(line)];
            } catch {
              return [];
            }
          }),
      );
  } catch {
    return [];
  }
}

function recordTimestamp(record) {
  if (record.time_unix_nano) return Number(BigInt(record.time_unix_nano) / 1_000_000n);
  return new Date(record.timestamp ?? 0).getTime();
}

function recordName(record) {
  return record.body ?? record.name;
}

function recordAttribute(record, key) {
  if (record.attributes?.[key] !== undefined) return record.attributes[key];
  const mapping = {
    "zed.cohort": "cohort",
    "zed.lane": "lane",
    "zed.build_version": "buildVersion",
  };
  return mapping[key] ? record[mapping[key]] : undefined;
}

function evaluateIncidentThresholds(storeDir, latestRecord) {
  try {
    const latestTime = recordTimestamp(latestRecord);
    const group = [
      recordAttribute(latestRecord, "zed.cohort"),
      recordAttribute(latestRecord, "zed.lane"),
      recordAttribute(latestRecord, "zed.build_version"),
    ].join(":");
    const recent = loadEventRecords(storeDir).filter((record) => {
      const recordGroup = [
        recordAttribute(record, "zed.cohort"),
        recordAttribute(record, "zed.lane"),
        recordAttribute(record, "zed.build_version"),
      ].join(":");
      return recordGroup === group && latestTime - recordTimestamp(record) <= 30 * 60 * 1000;
    });

    const thresholds = [
      {
        failureClass: "repeated_acp_disconnect",
        count: recent.filter(
          (record) => recordName(record) === "acp.disconnect" && latestTime - recordTimestamp(record) <= 15 * 60 * 1000,
        ).length,
        threshold: 3,
      },
      {
        failureClass: "repeated_app_instability",
        count: recent.filter((record) => ["app.hang", "app.crash", "app.force_quit"].includes(recordName(record))).length,
        threshold: 2,
      },
      {
        failureClass: "repeated_remote_bootstrap_failure",
        count: recent.filter((record) => recordName(record) === "remote.bootstrap.failure").length,
        threshold: 2,
      },
      {
        failureClass: "session_continuity_failure",
        count: recent.filter((record) => recordName(record) === "session.continuity.failure").length,
        threshold: 1,
      },
      {
        failureClass: "sustained_resource_pressure",
        count: recent.filter(
          (record) =>
            recordName(record) === "resource.sample" &&
            recordAttribute(record, "resource.pressure") === "high" &&
            latestTime - recordTimestamp(record) <= 10 * 60 * 1000,
        ).length,
        threshold: 3,
      },
    ];

    for (const threshold of thresholds) {
      if (threshold.count >= threshold.threshold) {
        appendIncidentCandidate(storeDir, {
          failureClass: threshold.failureClass,
          group,
          count: threshold.count,
          threshold: threshold.threshold,
          timestampMs: latestTime,
          traceId: latestRecord.trace_id,
        });
      }
    }
  } catch {
    // Incident promotion must never interfere with event recording or Zed.
  }
}

function appendIncidentCandidate(storeDir, input) {
  const filePath = path.join(storeDir, "incidents.jsonl");
  const dedupeKey = `${input.failureClass}:${input.group}`;
  const existing = fs.existsSync(filePath)
    ? fs
        .readFileSync(filePath, "utf8")
        .split("\n")
        .filter(Boolean)
        .flatMap((line) => {
          try {
            return [JSON.parse(line)];
          } catch {
            return [];
          }
        })
    : [];
  const withinDay = existing.some(
    (incident) => incident.dedupe_key === dedupeKey && input.timestampMs - Date.parse(incident.opened_at) < 24 * 60 * 60 * 1000,
  );
  if (withinDay) return;

  const incidentId = crypto
    .createHash("sha256")
    .update(`${dedupeKey}:${Math.floor(input.timestampMs / (24 * 60 * 60 * 1000))}`)
    .digest("hex")
    .slice(0, 16);
  appendPrivateLine(filePath, {
    schema_version: 1,
    incident_id: `zed10x-${incidentId}`,
    status: "open",
    opened_at: new Date(input.timestampMs).toISOString(),
    failure_class: input.failureClass,
    cohort_lane_build: input.group,
    observed_count: input.count,
    threshold: input.threshold,
    trace_id: input.traceId,
    tracking_issue: TRACKING_ISSUE,
    dedupe_key: dedupeKey,
  });
}

export function buildComparison(records) {
  const groups = new Map();
  for (const record of records) {
    const cohort = recordAttribute(record, "zed.cohort") ?? "unknown";
    const lane = recordAttribute(record, "zed.lane") ?? "unknown";
    const buildVersion = recordAttribute(record, "zed.build_version") ?? "unknown";
    const key = `${cohort}\u0000${lane}\u0000${buildVersion}`;
    const group = groups.get(key) ?? {
      cohort,
      lane,
      build_version: buildVersion,
      sessions: 0,
      acp_completed: 0,
      acp_disconnects: 0,
      reconnects: 0,
      continuity_successes: 0,
      continuity_failures: 0,
      hangs: 0,
      crashes: 0,
      forced_quits: 0,
      peak_rss_bytes: 0,
      first_timestamp_ms: Number.POSITIVE_INFINITY,
      last_timestamp_ms: 0,
    };
    const name = recordName(record);
    if (name === "app.launch") group.sessions += 1;
    if (name === "acp.complete") group.acp_completed += 1;
    if (name === "acp.disconnect") group.acp_disconnects += 1;
    if (name === "acp.reconnect") group.reconnects += 1;
    if (name === "session.continuity.success") group.continuity_successes += 1;
    if (name === "session.continuity.failure") group.continuity_failures += 1;
    if (name === "app.hang") group.hangs += 1;
    if (name === "app.crash") group.crashes += 1;
    if (name === "app.force_quit") group.forced_quits += 1;
    group.peak_rss_bytes = Math.max(group.peak_rss_bytes, Number(recordAttribute(record, "process.rss_bytes") ?? 0));
    const timestamp = recordTimestamp(record);
    group.first_timestamp_ms = Math.min(group.first_timestamp_ms, timestamp);
    group.last_timestamp_ms = Math.max(group.last_timestamp_ms, timestamp);
    groups.set(key, group);
  }

  const resultGroups = [...groups.values()]
    .map((group) => ({
      ...group,
      observed_minutes: Number.isFinite(group.first_timestamp_ms)
        ? Math.max(0, Math.round((group.last_timestamp_ms - group.first_timestamp_ms) / 6000) / 10)
        : 0,
      failure_events: group.acp_disconnects + group.continuity_failures + group.hangs + group.crashes + group.forced_quits,
    }))
    .map(({ first_timestamp_ms, last_timestamp_ms, ...group }) => group)
    .sort((left, right) =>
      [left.cohort, left.lane, left.build_version].join(":").localeCompare([right.cohort, right.lane, right.build_version].join(":")),
    );

  const credible = resultGroups.length >= 2 && resultGroups.every((group) => group.sessions >= 5 && group.observed_minutes >= 30);
  let verdict = "insufficient_evidence";
  if (credible) {
    const control = resultGroups.find((group) => group.cohort === "zed");
    const canary = resultGroups.find((group) => group.cohort === "zed10x");
    if (control && canary) {
      const controlRate = control.failure_events / Math.max(1, control.sessions);
      const canaryRate = canary.failure_events / Math.max(1, canary.sessions);
      verdict = canaryRate <= controlRate * 0.75 ? "zed10x_materially_more_reliable" : canaryRate >= controlRate * 1.25 ? "zed10x_materially_less_reliable" : "no_material_difference";
    }
  }

  return {
    schema_version: 1,
    generated_at: new Date().toISOString(),
    tracking_issue: TRACKING_ISSUE,
    confidence: credible ? "moderate" : "low",
    verdict,
    groups: resultGroups,
  };
}

function parseArguments(argumentsList) {
  const positionals = [];
  const options = {};
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    if (!argument.startsWith("--")) {
      positionals.push(argument);
      continue;
    }
    const key = argument.slice(2);
    const value = argumentsList[index + 1];
    if (value === undefined || value.startsWith("--")) options[key] = true;
    else {
      options[key] = value;
      index += 1;
    }
  }
  return { positionals, options };
}

function baseEvent(options, name) {
  const attributes = {};
  if (options.project) attributes["project.id"] = hashProjectIdentifier(options.project);
  if (options["project-id"]) attributes["project.id"] = options["project-id"];
  if (options["session-id"]) attributes["session.id_hash"] = hashProjectIdentifier(options["session-id"]);
  if (options["duration-ms"]) attributes["duration.ms"] = Number(options["duration-ms"]);
  if (options.provider) attributes["acp.provider"] = options.provider;
  if (options["failure-class"]) attributes["failure.class"] = options["failure-class"];
  if (options["continuity-trigger"]) attributes["continuity.trigger"] = options["continuity-trigger"];
  if (options["continuity-result"]) attributes["continuity.result"] = options["continuity-result"];
  if (options["remote-host"]) attributes["remote.host"] = options["remote-host"];
  if (options["session-kind"]) attributes["session.kind"] = options["session-kind"];
  if (options.resumed !== undefined) attributes["session.resumed"] = ["1", "true", "yes"].includes(String(options.resumed).toLowerCase());
  return {
    name,
    cohort: options.cohort ?? "zed10x",
    lane: options.lane ?? "local",
    appVersion: options["app-version"] ?? "unknown",
    buildVersion: options["build-version"] ?? "unknown",
    gitCommit: options["git-commit"],
    traceparent: options.traceparent,
    attributes,
  };
}

function parseElapsedSeconds(value) {
  const parts = value.trim().split(":").map(Number);
  if (parts.some((part) => !Number.isFinite(part))) return 0;
  if (parts.length === 3) return parts[0] * 3600 + parts[1] * 60 + parts[2];
  if (parts.length === 2) return parts[0] * 60 + parts[1];
  return parts[0] ?? 0;
}

export function discoverProcessIds(processTable, executablePath) {
  return processTable
    .split("\n")
    .flatMap((line) => {
      const match = line.trim().match(/^(\d+)\s+(.+)$/);
      if (!match || match[2] !== executablePath) return [];
      const processId = Number(match[1]);
      return Number.isInteger(processId) && processId > 0 ? [processId] : [];
    });
}

function readPlistValue(plistPath, key) {
  try {
    return execFileSync("/usr/libexec/PlistBuddy", ["-c", `Print :${key}`, plistPath], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "unknown";
  }
}

function sampleProcess(processId) {
  try {
    const sample = execFileSync(
      "/bin/ps",
      ["-p", String(processId), "-o", "%cpu=,rss=,state=,etime="],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    ).trim();
    const match = sample.match(/^([0-9.]+)\s+(\d+)\s+(\S+)\s+(\S+)$/);
    if (!match) return undefined;
    return {
      cpuPercent: Number(match[1]),
      rssBytes: Number(match[2]) * 1024,
      state: match[3].slice(0, 1).toLowerCase(),
      elapsedSeconds: parseElapsedSeconds(match[4]),
    };
  } catch {
    return undefined;
  }
}

async function observeControl(options) {
  const storeDir = options.store ?? DEFAULT_STORE;
  const executablePath = options.executable ?? "/Applications/Zed.app/Contents/MacOS/zed";
  const plistPath = options.plist ?? "/Applications/Zed.app/Contents/Info.plist";
  const intervalSeconds = Math.max(2, Number(options["sample-seconds"] ?? 10));
  const observed = new Map();
  let stopping = false;
  process.once("SIGTERM", () => {
    stopping = true;
  });
  process.once("SIGINT", () => {
    stopping = true;
  });

  while (!stopping) {
    if (isDisabled(process.env, storeDir)) {
      await new Promise((resolve) => setTimeout(resolve, intervalSeconds * 1000));
      continue;
    }

    let processIds = [];
    try {
      const processTable = execFileSync("/bin/ps", ["-axo", "pid=,comm="], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      });
      processIds = discoverProcessIds(processTable, executablePath);
    } catch {
      await new Promise((resolve) => setTimeout(resolve, intervalSeconds * 1000));
      continue;
    }

    const current = new Set(processIds);
    for (const processId of processIds) {
      let traceparent = observed.get(processId);
      if (!traceparent) {
        traceparent = createTraceContext().traceparent;
        observed.set(processId, traceparent);
        appendEvent(
          {
            name: "app.launch",
            cohort: "zed",
            lane: "local",
            appVersion: readPlistValue(plistPath, "CFBundleShortVersionString"),
            buildVersion: readPlistValue(plistPath, "CFBundleVersion"),
            traceparent,
            attributes: { "app.pid": processId, "session.kind": "control_observer" },
          },
          { storeDir },
        );
      }
      const sample = sampleProcess(processId);
      if (sample) {
        appendEvent(
          {
            name: "resource.sample",
            cohort: "zed",
            lane: "local",
            appVersion: readPlistValue(plistPath, "CFBundleShortVersionString"),
            buildVersion: readPlistValue(plistPath, "CFBundleVersion"),
            traceparent,
            attributes: {
              "app.pid": processId,
              "process.cpu_percent": sample.cpuPercent,
              "process.rss_bytes": sample.rssBytes,
              "process.state": sample.state,
              "process.elapsed_seconds": sample.elapsedSeconds,
              "resource.pressure": sample.rssBytes >= 8 * 1024 * 1024 * 1024 ? "high" : "normal",
            },
          },
          { storeDir },
        );
      }
    }

    for (const [processId, traceparent] of observed) {
      if (current.has(processId)) continue;
      appendEvent(
        {
          name: "app.process_exit",
          cohort: "zed",
          lane: "local",
          appVersion: readPlistValue(plistPath, "CFBundleShortVersionString"),
          buildVersion: readPlistValue(plistPath, "CFBundleVersion"),
          traceparent,
          attributes: { "app.pid": processId, "exit.class": "observed_vanish" },
        },
        { storeDir },
      );
      observed.delete(processId);
    }
    await new Promise((resolve) => setTimeout(resolve, intervalSeconds * 1000));
  }
  return 0;
}

async function watchProcess(options) {
  const processId = Number(options.pid);
  if (!Number.isInteger(processId) || processId <= 0) return 2;
  const storeDir = options.store ?? DEFAULT_STORE;
  if (isDisabled(process.env, storeDir)) return 0;
  const intervalSeconds = Math.max(1, Number(options["sample-seconds"] ?? 10));
  const traceparent = options.traceparent ?? createTraceContext().traceparent;
  const tracedOptions = { ...options, traceparent };
  const appEvent = baseEvent(tracedOptions, "app.launch");
  appEvent.attributes["app.pid"] = processId;
  if (!appendEvent(appEvent, { storeDir }) && process.env.ZED_10X_CANARY_DEBUG === "1") {
    process.stderr.write("canary_event_rejected:app.launch\n");
  }

  while (true) {
    if (isDisabled(process.env, storeDir)) return 0;
    let sample;
    try {
      sample = execFileSync(
        "/bin/ps",
        ["-p", String(processId), "-o", "%cpu=,rss=,state=,etime="],
        { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
      ).trim();
    } catch {
      const exitEvent = baseEvent(tracedOptions, "app.process_exit");
      exitEvent.attributes["app.pid"] = processId;
      exitEvent.attributes["exit.class"] = "observed_vanish";
      appendEvent(exitEvent, { storeDir });
      return 0;
    }

    if (!sample) return 0;
    const match = sample.match(/^([0-9.]+)\s+(\d+)\s+(\S+)\s+(\S+)$/);
    if (match) {
      const resourceEvent = baseEvent(tracedOptions, "resource.sample");
      resourceEvent.attributes = {
        "app.pid": processId,
        "process.cpu_percent": Number(match[1]),
        "process.rss_bytes": Number(match[2]) * 1024,
        "process.state": match[3].slice(0, 1).toLowerCase(),
        "process.elapsed_seconds": parseElapsedSeconds(match[4]),
        "resource.pressure": Number(match[2]) >= 8 * 1024 * 1024 ? "high" : "normal",
      };
      appendEvent(resourceEvent, { storeDir });
    }
    await new Promise((resolve) => setTimeout(resolve, intervalSeconds * 1000));
  }
}

function writeComparison(storeDir, outputPath) {
  const comparison = buildComparison(loadEventRecords(storeDir));
  const body = `${JSON.stringify(comparison, null, 2)}\n`;
  if (outputPath) {
    fs.writeFileSync(outputPath, body, { mode: 0o600 });
    fs.chmodSync(outputPath, 0o600);
  } else {
    process.stdout.write(body);
  }
}

async function main() {
  const { positionals, options } = parseArguments(process.argv.slice(2));
  const command = positionals[0];
  const storeDir = options.store ?? DEFAULT_STORE;
  if (command === "watch") return watchProcess(options);
  if (command === "observe-control") return observeControl(options);
  if (command === "record") {
    const input = baseEvent(options, positionals[1]);
    return appendEvent(input, { storeDir }) ? 0 : 1;
  }
  if (command === "compare") {
    writeComparison(storeDir, options.output);
    return 0;
  }
  if (command === "disable") {
    ensurePrivateDirectory(storeDir);
    fs.writeFileSync(path.join(storeDir, "DISABLED"), "disabled by operator\n", { mode: 0o600 });
    return 0;
  }
  if (command === "enable") {
    fs.rmSync(path.join(storeDir, "DISABLED"), { force: true });
    return 0;
  }
  if (command === "status") {
    process.stdout.write(`${isDisabled(process.env, storeDir) ? "disabled" : "enabled"}\n`);
    return 0;
  }
  process.stderr.write("usage: zed-10x-canary.mjs <watch|observe-control|record|compare|disable|enable|status> [options]\n");
  return 2;
}

function isMainModule() {
  if (!process.argv[1]) return false;
  try {
    return fs.realpathSync(fileURLToPath(import.meta.url)) === fs.realpathSync(process.argv[1]);
  } catch {
    return false;
  }
}

if (isMainModule()) {
  process.exitCode = await main();
}

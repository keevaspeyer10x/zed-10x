#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const API_ROOT = "https://api.github.com/repos/zed-industries/zed";
const TRACKING_ISSUE = "https://github.com/keevaspeyer10x/zed-10x/issues/6";
const TRACKED_ISSUES = [47003, 51597, 58331, 58444, 59323, 59394, 60134, 60213, 60413, 61214, 61386];
const TRACKED_SOURCE_PATHS = [
  "crates/project/src/project.rs",
  "crates/settings/src/editorconfig_store.rs",
  "crates/settings/src/settings_store.rs",
];
const TRACKED_PATCHES = [
  {
    patchId: "P-CPU-MULTIWORKTREE",
    pullNumber: 58984,
  },
];

async function defaultFetchJson(url) {
  const headers = {
    accept: "application/vnd.github+json",
    "user-agent": "zed-10x-upstream-exit-monitor",
    "x-github-api-version": "2022-11-28",
  };
  if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const response = await fetch(url, { headers, redirect: "error", signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error(`GitHub API ${response.status} for ${new URL(url).pathname}`);
  return response.json();
}

function issueProjection(issue) {
  if (!Number.isInteger(issue.number) || !["open", "closed"].includes(issue.state)) {
    throw new Error("upstream issue response is malformed");
  }
  return {
    number: issue.number,
    state: issue.state,
    stateReason: issue.state_reason ?? null,
    updatedAt: issue.updated_at,
    url: issue.html_url,
  };
}

function pullProjection(pull) {
  if (!Number.isInteger(pull.number) || !["open", "closed"].includes(pull.state)) {
    throw new Error("upstream pull response is malformed");
  }
  return {
    mergeCommitSha: pull.merge_commit_sha ?? null,
    mergedAt: pull.merged_at ?? null,
    number: pull.number,
    state: pull.state,
    updatedAt: pull.updated_at,
    url: pull.html_url,
  };
}

function sourceProjection(pathname, commits) {
  const latest = Array.isArray(commits) ? commits[0] : null;
  if (
    !latest ||
    typeof latest.sha !== "string" ||
    typeof latest.commit?.committer?.date !== "string" ||
    typeof latest.html_url !== "string"
  ) {
    throw new Error(`upstream source history is malformed for ${pathname}`);
  }
  return {
    committedAt: latest.commit.committer.date,
    latestCommitSha: latest.sha,
    path: pathname,
    url: latest.html_url,
  };
}

export async function evaluateUpstreamExit({ fetchJson = defaultFetchJson, observedAt = new Date().toISOString() } = {}) {
  const issues = [];
  for (const number of TRACKED_ISSUES) {
    issues.push(issueProjection(await fetchJson(`${API_ROOT}/issues/${number}`)));
  }

  const sourceWatches = [];
  for (const pathname of TRACKED_SOURCE_PATHS) {
    sourceWatches.push(
      sourceProjection(
        pathname,
        await fetchJson(`${API_ROOT}/commits?path=${encodeURIComponent(pathname)}&per_page=1`),
      ),
    );
  }

  let latestRelease = null;
  try {
    const release = await fetchJson(`${API_ROOT}/releases/latest`);
    if (typeof release.tag_name !== "string" || typeof release.published_at !== "string") {
      throw new Error("upstream latest release response is malformed");
    }
    latestRelease = {
      publishedAt: release.published_at,
      tagName: release.tag_name,
      url: release.html_url,
    };
  } catch (error) {
    latestRelease = { errorClass: error.constructor.name };
  }

  const patches = [
    {
      patchId: "P-001-REMOTE-SETTINGS-INVALIDATION",
      patchRemovalAuthorized: false,
      sourceWatches,
      state: "TRACKING",
    },
  ];
  for (const tracked of TRACKED_PATCHES) {
    const pull = pullProjection(await fetchJson(`${API_ROOT}/pulls/${tracked.pullNumber}`));
    let state = "TRACKING";
    let releaseContainsMerge = false;
    if (pull.mergedAt && pull.mergeCommitSha) {
      state = "UPSTREAM_CANDIDATE";
      if (latestRelease?.tagName) {
        const comparison = await fetchJson(
          `${API_ROOT}/compare/${pull.mergeCommitSha}...${encodeURIComponent(latestRelease.tagName)}`,
        );
        if (!["ahead", "behind", "diverged", "identical"].includes(comparison.status)) {
          throw new Error("upstream comparison response is malformed");
        }
        releaseContainsMerge = comparison.status === "ahead" || comparison.status === "identical";
        if (releaseContainsMerge) state = "READY_FOR_CANARY";
      }
    }
    patches.push({
      patchId: tracked.patchId,
      patchRemovalAuthorized: false,
      pull,
      releaseContainsMerge,
      state,
    });
  }

  return {
    schema: "zed-10x-upstream-exit-monitor-v1",
    observedAt,
    trackingIssue: TRACKING_ISSUE,
    upstreamRepository: "zed-industries/zed",
    issues,
    latestRelease,
    patches,
    retirementAuthorized: false,
    requiredCanaryEvidence: [
      "focused regression",
      "official Zed versus Zed 10x A/B",
      "Isla multi-repo and multi-worktree reconnect soak",
      "CPU, memory, and session-integrity evidence",
      "rollback proof",
    ],
  };
}

function parseArguments(args) {
  if (args.length !== 2 || args[0] !== "--output") {
    throw new Error("Usage: zed-10x-upstream-exit-monitor.mjs --output <receipt.json>");
  }
  return path.resolve(args[1]);
}

function writeReceipt(outputPath, receipt) {
  const parent = path.dirname(outputPath);
  const metadata = fs.lstatSync(parent);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error("receipt parent must be a real directory");
  }
  const descriptor = fs.openSync(
    outputPath,
    fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW,
    0o600,
  );
  try {
    fs.writeFileSync(descriptor, `${JSON.stringify(receipt, null, 2)}\n`);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
  const directoryDescriptor = fs.openSync(parent, fs.constants.O_RDONLY);
  try {
    fs.fsyncSync(directoryDescriptor);
  } finally {
    fs.closeSync(directoryDescriptor);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const outputPath = parseArguments(process.argv.slice(2));
    const receipt = await evaluateUpstreamExit();
    writeReceipt(outputPath, receipt);
    process.stdout.write(`${JSON.stringify({ outputPath, schema: receipt.schema })}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}

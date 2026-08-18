import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { evaluateUpstreamExit } from "../zed-10x-upstream-exit-monitor.mjs";

function fixtureFetch({ merged = false, comparisonStatus = "behind" } = {}) {
  return async (url) => {
    const pathname = new URL(url).pathname;
    const issue = pathname.match(/\/issues\/(\d+)$/);
    if (issue) {
      const number = Number(issue[1]);
      return {
        number,
        state: number === 60213 ? "open" : "closed",
        state_reason: number === 60213 ? null : "completed",
        updated_at: "2026-08-18T00:00:00Z",
        html_url: `https://github.com/zed-industries/zed/issues/${number}`,
      };
    }
    if (pathname.endsWith("/pulls/58984")) {
      return {
        number: 58984,
        state: merged ? "closed" : "open",
        merged_at: merged ? "2026-08-17T00:00:00Z" : null,
        merge_commit_sha: merged ? "0123456789abcdef0123456789abcdef01234567" : null,
        updated_at: "2026-08-18T00:00:00Z",
        html_url: "https://github.com/zed-industries/zed/pull/58984",
      };
    }
    if (pathname.endsWith("/releases/latest")) {
      return {
        tag_name: "v0.205.0",
        published_at: "2026-08-18T00:00:00Z",
        html_url: "https://github.com/zed-industries/zed/releases/tag/v0.205.0",
      };
    }
    if (pathname.endsWith("/commits")) {
      const sourcePath = new URL(url).searchParams.get("path");
      return [
        {
          sha: "fedcba9876543210fedcba9876543210fedcba98",
          commit: { committer: { date: "2026-08-17T00:00:00Z" } },
          html_url: `https://github.com/zed-industries/zed/commit/${path.basename(sourcePath)}`,
        },
      ];
    }
    if (pathname.includes("/compare/")) return { status: comparisonStatus };
    throw new Error(`unexpected fixture URL ${url}`);
  };
}

test("an open upstream PR remains tracking and cannot retire the fork", async () => {
  const receipt = await evaluateUpstreamExit({
    fetchJson: fixtureFetch(),
    observedAt: "2026-08-18T00:00:00Z",
  });

  const cpuPatch = receipt.patches.find((patch) => patch.patchId === "P-CPU-MULTIWORKTREE");
  assert.equal(cpuPatch.state, "TRACKING");
  assert.equal(cpuPatch.patchRemovalAuthorized, false);
  assert.equal(receipt.retirementAuthorized, false);
  assert.equal(receipt.trackingIssue, "https://github.com/keevaspeyer10x/zed-10x/issues/6");
  assert.deepEqual(
    receipt.issues.map((issue) => issue.number),
    [47003, 51597, 58331, 58444, 59323, 59394, 60134, 60213, 60413, 61214, 61386],
  );
  const settingsPatch = receipt.patches.find(
    (patch) => patch.patchId === "P-001-REMOTE-SETTINGS-INVALIDATION",
  );
  assert.equal(settingsPatch.sourceWatches.length, 3);
  assert.deepEqual(
    settingsPatch.sourceWatches.map((watch) => watch.path),
    [
      "crates/project/src/project.rs",
      "crates/settings/src/editorconfig_store.rs",
      "crates/settings/src/settings_store.rs",
    ],
  );
});

test("a merged and released patch advances only to ready for canary", async () => {
  const receipt = await evaluateUpstreamExit({
    fetchJson: fixtureFetch({ merged: true, comparisonStatus: "ahead" }),
    observedAt: "2026-08-18T00:00:00Z",
  });

  const patch = receipt.patches.find((candidate) => candidate.patchId === "P-CPU-MULTIWORKTREE");
  assert.equal(patch.state, "READY_FOR_CANARY");
  assert.equal(patch.releaseContainsMerge, true);
  assert.equal(patch.patchRemovalAuthorized, false);
  assert.equal(receipt.retirementAuthorized, false);
  assert.ok(receipt.requiredCanaryEvidence.includes("rollback proof"));
});

test("the weekly workflow is read-only and retains the monitor receipt", () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const workflow = fs.readFileSync(path.join(root, ".github/workflows/zed-10x-upstream-exit.yml"), "utf8");

  assert.match(workflow, /schedule:[\s\S]*cron: "17 3 \* \* 1"/);
  assert.match(workflow, /permissions:[\s\S]*contents: read/);
  assert.doesNotMatch(workflow, /issues: write|pull-requests: write|contents: write/);
  assert.match(workflow, /zed-10x-upstream-exit-monitor\.mjs\s*\\?\s*--output/);
  assert.match(workflow, /actions\/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/);
});

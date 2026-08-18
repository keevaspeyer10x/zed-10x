#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const EXPECTED_FILES = [
  "Zed-10x-aarch64.dmg",
  "zed-10x-release-receipt.json",
  "zed-remote-server-linux-aarch64.gz",
  "zed-remote-server-linux-x86_64.gz",
  "zed-remote-server-macos-aarch64.gz",
];

function requireRegularFile(filePath, label) {
  const metadata = fs.lstatSync(filePath);
  if (metadata.isSymbolicLink() || !metadata.isFile()) {
    throw new Error(`${label} must be a real regular file`);
  }
}

function sha256File(filePath) {
  const digest = crypto.createHash("sha256");
  const descriptor = fs.openSync(filePath, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const buffer = Buffer.allocUnsafe(1024 * 1024);
    for (;;) {
      const bytesRead = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      digest.update(buffer.subarray(0, bytesRead));
    }
  } finally {
    fs.closeSync(descriptor);
  }
  return digest.digest("hex");
}

function expectedAssetDigests(directory) {
  return new Map(
    EXPECTED_FILES.map((name) => {
      const filePath = path.join(directory, name);
      return [
        name,
        {
          digest: `sha256:${sha256File(filePath)}`,
          size: fs.statSync(filePath).size,
        },
      ];
    }),
  );
}

export function verifyGitHubRelease({
  directory,
  expectedCommit,
  expectedVersion,
  release,
  expectedState,
}) {
  const expectedTag = `zed-10x-v${expectedVersion}`;
  if (release.tag_name !== expectedTag || release.target_commitish !== expectedCommit) {
    throw new Error("GitHub release tag or target commit mismatch");
  }
  if (release.prerelease !== false) {
    throw new Error("Zed 10x GitHub release must not be a prerelease");
  }
  if (expectedState === "draft" || expectedState === "draft-uploaded") {
    if (release.draft !== true || release.immutable !== false) {
      throw new Error("existing Zed 10x release is not a resumable draft");
    }
    if (expectedState === "draft") {
      return { expectedCommit, expectedTag, state: "draft" };
    }
  }
  if (expectedState !== "draft-uploaded" && expectedState !== "immutable") {
    throw new Error("expected GitHub release state must be draft, draft-uploaded, or immutable");
  }
  if (expectedState === "immutable" && (release.draft !== false || release.immutable !== true)) {
    throw new Error("published Zed 10x release is not immutable");
  }

  const expectedAssets = expectedAssetDigests(directory);
  if (!Array.isArray(release.assets) || release.assets.length !== expectedAssets.size) {
    throw new Error("published Zed 10x release has an unexpected asset count");
  }
  for (const asset of release.assets) {
    const expected = expectedAssets.get(asset.name);
    if (
      !expected ||
      asset.state !== "uploaded" ||
      asset.digest !== expected.digest ||
      asset.size !== expected.size
    ) {
      throw new Error(`published Zed 10x asset mismatch: ${asset.name ?? "unnamed"}`);
    }
    expectedAssets.delete(asset.name);
  }
  if (expectedAssets.size !== 0) {
    throw new Error("published Zed 10x release is missing an expected asset");
  }
  return { expectedCommit, expectedTag, state: expectedState };
}

export function verifyPublication({ directory, expectedCommit, expectedVersion }) {
  if (!/^[0-9a-f]{40}$/.test(expectedCommit)) {
    throw new Error("expected commit must be a full lowercase Git SHA");
  }
  if (!/^\d+\.\d+\.\d+\+dev\.\d+\.[0-9a-f]{40}$/.test(expectedVersion)) {
    throw new Error("expected version must be a Zed 10x release semantic version");
  }
  if (!expectedVersion.endsWith(`.${expectedCommit}`)) {
    throw new Error("release version is not bound to the expected commit");
  }

  const directoryMetadata = fs.lstatSync(directory);
  if (directoryMetadata.isSymbolicLink() || !directoryMetadata.isDirectory()) {
    throw new Error("release directory must be a real directory");
  }
  const entries = fs.readdirSync(directory).sort();
  if (entries.join("\n") !== [...EXPECTED_FILES].sort().join("\n")) {
    throw new Error(`release directory contains an unexpected file set: ${entries.join(",")}`);
  }
  for (const name of EXPECTED_FILES) {
    requireRegularFile(path.join(directory, name), name);
  }

  const receiptPath = path.join(directory, "zed-10x-release-receipt.json");
  const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
  if (receipt.schema !== "zed-10x-macos-release-verification-v1" || receipt.status !== "verified") {
    throw new Error("release receipt is not a verified Zed 10x receipt");
  }
  if (receipt.sourceCommit !== expectedCommit) {
    throw new Error("release receipt source commit mismatch");
  }
  const dmgSha256 = sha256File(path.join(directory, "Zed-10x-aarch64.dmg"));
  const remoteServerSha256 = sha256File(path.join(directory, "zed-remote-server-macos-aarch64.gz"));
  if (receipt.dmgSha256 !== dmgSha256) {
    throw new Error("disk image digest does not match the signed receipt");
  }
  if (receipt.remoteServerSha256 !== remoteServerSha256) {
    throw new Error("remote server digest does not match the signed receipt");
  }
  return { dmgSha256, expectedCommit, expectedVersion, remoteServerSha256 };
}

function parseArguments(args) {
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!key?.startsWith("--") || value === undefined || values.has(key)) {
      throw new Error("expected unique paired --name value arguments");
    }
    values.set(key, value);
  }
  const required = new Set(["--directory", "--expected-commit", "--expected-version"]);
  const optional = new Set(["--github-release-json", "--expected-state"]);
  const allowed = new Set([...required, ...optional]);
  for (const key of values.keys()) {
    if (!allowed.has(key)) throw new Error(`unknown argument ${key}`);
  }
  for (const key of required) {
    if (!values.has(key)) throw new Error(`missing argument ${key}`);
  }
  if (values.has("--github-release-json") !== values.has("--expected-state")) {
    throw new Error("GitHub release JSON and expected state must be supplied together");
  }
  return {
    directory: path.resolve(values.get("--directory")),
    expectedCommit: values.get("--expected-commit"),
    expectedVersion: values.get("--expected-version"),
    githubReleaseJson: values.has("--github-release-json")
      ? path.resolve(values.get("--github-release-json"))
      : null,
    expectedState: values.get("--expected-state") ?? null,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const args = parseArguments(process.argv.slice(2));
    const local = verifyPublication(args);
    let github = null;
    if (args.githubReleaseJson) {
      requireRegularFile(args.githubReleaseJson, "GitHub release JSON");
      const release = JSON.parse(fs.readFileSync(args.githubReleaseJson, "utf8"));
      github = verifyGitHubRelease({ ...args, release });
    }
    process.stdout.write(`${JSON.stringify({ github, local })}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}

#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { execFile } from "node:child_process";
import { pipeline } from "node:stream/promises";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { createGunzip } from "node:zlib";

const execFileAsync = promisify(execFile);
const EXPECTED_BUNDLE_IDENTIFIER = "ai.10xlabs.Zed10x";
const EXPECTED_BUNDLE_NAME = "Zed 10x";
const EXPECTED_EXECUTABLE = "zed-10x-launcher";
const EXPECTED_URL_SCHEME = "zed-10x";
const EXPECTED_APP_FILENAME = "Zed 10x.app";
const SHIPPED_EXECUTABLES = ["zed-10x-runtime", "cli", "git"];

async function defaultRunCommand(command, args) {
  try {
    const result = await execFileAsync(command, args, {
      encoding: "utf8",
      maxBuffer: 4 * 1024 * 1024,
    });
    return { stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    const stderr = typeof error.stderr === "string" ? error.stderr.trim() : "";
    const detail = stderr ? `: ${stderr}` : "";
    throw new Error(`${path.basename(command)} failed${detail}`, { cause: error });
  }
}

function requireDirectory(directoryPath, label) {
  const metadata = fs.lstatSync(directoryPath);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error(`${label} must be a real directory`);
  }
}

function requireRegularFile(filePath, label) {
  const metadata = fs.lstatSync(filePath);
  if (metadata.isSymbolicLink() || !metadata.isFile()) {
    throw new Error(`${label} must be a real regular file`);
  }
}

function pathEntryExists(filePath) {
  try {
    fs.lstatSync(filePath);
    return true;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

async function defaultReadPlist(plistPath, key, runCommand) {
  const result = await runCommand("/usr/libexec/PlistBuddy", ["-c", `Print ${key}`, plistPath]);
  return result.stdout.trim();
}

async function signatureDetails(codePath, runCommand) {
  const result = await runCommand("/usr/bin/codesign", ["--display", "--verbose=4", codePath]);
  const details = `${result.stdout}\n${result.stderr}`;
  const teamId = details.match(/^TeamIdentifier=(.+)$/m)?.[1]?.trim();
  const authority = details.match(/^Authority=(.+)$/m)?.[1]?.trim();
  const hardenedRuntime = /^CodeDirectory .*flags=.*\([^)]*\bruntime\b[^)]*\)/m.test(details);
  return { authority, hardenedRuntime, teamId };
}

async function verifyCodeObject(codePath, expectedTeamId, runCommand) {
  requireRegularFile(codePath, "shipped executable");
  await runCommand("/usr/bin/codesign", ["--verify", "--strict", "--verbose=4", "--all-architectures", codePath]);
  const signature = await signatureDetails(codePath, runCommand);
  if (signature.teamId !== expectedTeamId) {
    throw new Error(`shipped executable Developer ID team mismatch: ${signature.teamId ?? "missing"}`);
  }
  if (!signature.authority?.startsWith("Developer ID Application:")) {
    throw new Error("shipped executable is not signed with Developer ID Application");
  }
  if (!signature.hardenedRuntime) {
    throw new Error("shipped executable signature does not enable hardened runtime");
  }
}

export async function verifyAppBundle({
  appPath,
  expectedCommit,
  expectedTeamId,
  readPlist = defaultReadPlist,
  runCommand = defaultRunCommand,
}) {
  if (!/^[0-9a-f]{40}$/.test(expectedCommit)) {
    throw new Error("expected commit must be a full lowercase Git SHA");
  }
  if (!/^[A-Z0-9]{10}$/.test(expectedTeamId)) {
    throw new Error("expected Developer ID team must be ten uppercase characters");
  }

  requireDirectory(appPath, "application bundle");
  const contentsPath = path.join(appPath, "Contents");
  const plistPath = path.join(contentsPath, "Info.plist");
  requireRegularFile(plistPath, "Info.plist");
  if (pathEntryExists(path.join(contentsPath, "embedded.provisionprofile"))) {
    throw new Error("fork release must not embed the upstream provisioning profile");
  }

  const plist = async (key) => readPlist(plistPath, key, runCommand);
  const bundleIdentifier = await plist(":CFBundleIdentifier");
  const bundleName = await plist(":CFBundleName");
  const executable = await plist(":CFBundleExecutable");
  const urlScheme = await plist(":CFBundleURLTypes:0:CFBundleURLSchemes:0");
  if (bundleIdentifier !== EXPECTED_BUNDLE_IDENTIFIER) {
    throw new Error(`bundle identifier mismatch: ${bundleIdentifier}`);
  }
  if (bundleName !== EXPECTED_BUNDLE_NAME) {
    throw new Error(`bundle name mismatch: ${bundleName}`);
  }
  if (executable !== EXPECTED_EXECUTABLE) {
    throw new Error(`bundle executable mismatch: ${executable}`);
  }
  if (urlScheme !== EXPECTED_URL_SCHEME) {
    throw new Error(`bundle URL scheme mismatch: ${urlScheme}`);
  }

  const revisionPath = path.join(contentsPath, "Resources", "zed-10x-git-commit");
  requireRegularFile(revisionPath, "source revision marker");
  const sourceCommit = fs.readFileSync(revisionPath, "utf8").trim();
  if (sourceCommit !== expectedCommit) {
    throw new Error(`source revision mismatch: ${sourceCommit}`);
  }

  for (const executableName of SHIPPED_EXECUTABLES) {
    await verifyCodeObject(path.join(contentsPath, "MacOS", executableName), expectedTeamId, runCommand);
  }
  await runCommand("/usr/bin/codesign", ["--verify", "--strict", "--verbose=4", "--all-architectures", appPath]);
  const signature = await signatureDetails(appPath, runCommand);
  if (signature.teamId !== expectedTeamId) {
    throw new Error(`Developer ID team mismatch: ${signature.teamId ?? "missing"}`);
  }
  if (!signature.authority?.startsWith("Developer ID Application:")) {
    throw new Error("application is not signed with Developer ID Application");
  }
  if (!signature.hardenedRuntime) {
    throw new Error("application signature does not enable hardened runtime");
  }
  await runCommand("/usr/sbin/spctl", ["--assess", "--type", "execute", "--verbose=4", appPath]);

  return {
    bundleIdentifier,
    bundleName,
    executable,
    hardenedRuntime: true,
    sourceCommit,
    teamId: signature.teamId,
    urlScheme,
  };
}

async function sha256File(filePath) {
  const digest = crypto.createHash("sha256");
  const stream = fs.createReadStream(filePath);
  for await (const chunk of stream) {
    digest.update(chunk);
  }
  return digest.digest("hex");
}

function writeReceipt(receiptPath, receipt) {
  const parent = path.dirname(receiptPath);
  requireDirectory(parent, "receipt parent");
  const temporary = path.join(parent, `.${path.basename(receiptPath)}.${process.pid}.${crypto.randomUUID()}.tmp`);
  const bytes = `${JSON.stringify(receipt, null, 2)}\n`;
  let descriptor;
  try {
    descriptor = fs.openSync(
      temporary,
      fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW,
      0o600,
    );
    fs.writeFileSync(descriptor, bytes, "utf8");
    fs.fsyncSync(descriptor);
    fs.closeSync(descriptor);
    descriptor = undefined;
    fs.linkSync(temporary, receiptPath);
    const directoryDescriptor = fs.openSync(parent, fs.constants.O_RDONLY);
    try {
      fs.fsyncSync(directoryDescriptor);
    } finally {
      fs.closeSync(directoryDescriptor);
    }
    fs.unlinkSync(temporary);
    const cleanupDirectoryDescriptor = fs.openSync(parent, fs.constants.O_RDONLY);
    try {
      fs.fsyncSync(cleanupDirectoryDescriptor);
    } finally {
      fs.closeSync(cleanupDirectoryDescriptor);
    }
  } finally {
    if (descriptor !== undefined) {
      fs.closeSync(descriptor);
    }
    try {
      fs.unlinkSync(temporary);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}

export async function verifyRelease({
  dmgPath,
  remoteServerPath,
  expectedCommit,
  expectedTeamId,
  receiptPath,
  readPlist = defaultReadPlist,
  runCommand = defaultRunCommand,
}) {
  requireRegularFile(dmgPath, "release disk image");
  requireRegularFile(remoteServerPath, "compressed remote server");
  await runCommand("/usr/bin/codesign", ["--verify", "--strict", "--verbose=4", dmgPath]);
  const diskSignature = await signatureDetails(dmgPath, runCommand);
  if (diskSignature.teamId !== expectedTeamId) {
    throw new Error(`disk image Developer ID team mismatch: ${diskSignature.teamId ?? "missing"}`);
  }
  if (!diskSignature.authority?.startsWith("Developer ID Application:")) {
    throw new Error("disk image is not signed with Developer ID Application");
  }
  await runCommand("/usr/bin/xcrun", ["stapler", "validate", dmgPath]);
  await runCommand("/usr/sbin/spctl", [
    "--assess",
    "--type",
    "open",
    "--context",
    "context:primary-signature",
    "--verbose=4",
    dmgPath,
  ]);

  const temporaryRoot = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-verify-"));
  const mountPath = path.join(temporaryRoot, "mounted");
  fs.mkdirSync(mountPath, { mode: 0o700 });
  let attached = false;
  let app;
  let verificationError;
  try {
    const remoteServerExecutable = path.join(temporaryRoot, "zed-remote-server");
    await pipeline(
      fs.createReadStream(remoteServerPath),
      createGunzip(),
      fs.createWriteStream(remoteServerExecutable, { flags: "wx", mode: 0o600 }),
    );
    await verifyCodeObject(remoteServerExecutable, expectedTeamId, runCommand);

    await runCommand("/usr/bin/hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mountPath, dmgPath]);
    attached = true;
    const appNames = fs
      .readdirSync(mountPath, { withFileTypes: true })
      .filter((entry) => entry.name.endsWith(".app"))
      .map((entry) => entry.name);
    if (appNames.length !== 1 || appNames[0] !== EXPECTED_APP_FILENAME) {
      throw new Error(`disk image app mismatch: ${appNames.join(",") || "missing"}`);
    }
    app = await verifyAppBundle({
      appPath: path.join(mountPath, EXPECTED_APP_FILENAME),
      expectedCommit,
      expectedTeamId,
      readPlist,
      runCommand,
    });
  } catch (error) {
    verificationError = error;
  } finally {
    let detached = !attached;
    if (attached) {
      try {
        await runCommand("/usr/bin/hdiutil", ["detach", mountPath]);
        detached = true;
      } catch (detachError) {
        try {
          await runCommand("/usr/bin/hdiutil", ["detach", "-force", mountPath]);
          detached = true;
        } catch (forceDetachError) {
          verificationError = new Error("unable to detach the release disk image", {
            cause: forceDetachError,
          });
        }
      }
    }
    if (detached) {
      fs.rmSync(temporaryRoot, { recursive: true, force: true });
    }
  }
  if (verificationError !== undefined) throw verificationError;

  const receipt = {
    schema: "zed-10x-macos-release-verification-v1",
    status: "verified",
    observedAt: new Date().toISOString(),
    bundleIdentifier: app.bundleIdentifier,
    bundleName: app.bundleName,
    dmgSha256: await sha256File(dmgPath),
    hardenedRuntime: app.hardenedRuntime,
    remoteServerSha256: await sha256File(remoteServerPath),
    sourceCommit: app.sourceCommit,
    teamId: app.teamId,
    urlScheme: app.urlScheme,
  };
  writeReceipt(receiptPath, receipt);
  return receipt;
}

function parseArguments(args) {
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error("expected paired --name value arguments");
    }
    if (values.has(key)) throw new Error(`duplicate argument ${key}`);
    values.set(key, value);
  }
  const allowed = new Set(["--dmg", "--remote-server", "--expected-commit", "--expected-team-id", "--receipt"]);
  for (const key of values.keys()) {
    if (!allowed.has(key)) throw new Error(`unknown argument ${key}`);
  }
  for (const key of allowed) {
    if (!values.has(key)) throw new Error(`missing argument ${key}`);
  }
  return {
    dmgPath: path.resolve(values.get("--dmg")),
    remoteServerPath: path.resolve(values.get("--remote-server")),
    expectedCommit: values.get("--expected-commit"),
    expectedTeamId: values.get("--expected-team-id"),
    receiptPath: path.resolve(values.get("--receipt")),
  };
}

async function main() {
  const receipt = await verifyRelease(parseArguments(process.argv.slice(2)));
  process.stdout.write(`${JSON.stringify(receipt)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}

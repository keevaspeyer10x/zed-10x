import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { gzipSync } from "node:zlib";

import { verifyAppBundle, verifyRelease } from "../verify-zed-10x-macos-release.mjs";

const TEAM_ID = "A1B2C3D4E5";
const COMMIT = "0123456789abcdef0123456789abcdef01234567";

function appFixture(root, name = "Zed 10x.app") {
  const appPath = path.join(root, name);
  fs.mkdirSync(path.join(appPath, "Contents", "MacOS"), { recursive: true });
  fs.mkdirSync(path.join(appPath, "Contents", "Resources"), {
    recursive: true,
  });
  for (const executable of ["zed-10x-launcher", "zed-10x-runtime", "cli", "git"]) {
    fs.writeFileSync(path.join(appPath, "Contents", "MacOS", executable), "x");
  }
  fs.writeFileSync(path.join(appPath, "Contents", "Resources", "zed-10x-git-commit"), `${COMMIT}\n`);
  fs.writeFileSync(path.join(appPath, "Contents", "Info.plist"), "fixture");
  return appPath;
}

function remoteServerFixture(root) {
  const remoteServerPath = path.join(root, "zed-remote-server-macos-aarch64.gz");
  fs.writeFileSync(remoteServerPath, gzipSync("signed-remote-server-fixture"));
  return remoteServerPath;
}

function plistReader(overrides = {}) {
  const values = {
    ":CFBundleIdentifier": "ai.10xlabs.Zed10x",
    ":CFBundleName": "Zed 10x",
    ":CFBundleExecutable": "zed-10x-launcher",
    ":CFBundleURLTypes:0:CFBundleURLSchemes:0": "zed-10x",
    ...overrides,
  };
  return async (_plistPath, key) => {
    assert.ok(key in values, `unexpected plist key ${key}`);
    return values[key];
  };
}

function successfulRunner(commands, detail = {}) {
  return async (command, args) => {
    commands.push([command, ...args]);
    if (command === "/usr/bin/codesign" && args[0] === "--display") {
      return {
        stdout: "",
        stderr: [
          "Executable=/fixture",
          "Identifier=ai.10xlabs.Zed10x",
          "CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=3+7 location=embedded",
          `Authority=${detail.authority ?? `Developer ID Application: 10x Labs (${detail.teamId ?? TEAM_ID})`}`,
          `TeamIdentifier=${detail.teamId ?? TEAM_ID}`,
        ].join("\n"),
      };
    }
    return { stdout: "", stderr: "" };
  };
}

function fakeReleaseEnvironment(overrides = {}) {
  return {
    HOME: os.homedir(),
    PATH: "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
    ZED_10X_MACOS_CERTIFICATE: "dGVzdA==",
    ZED_10X_MACOS_CERTIFICATE_PASSWORD: "test-password",
    ZED_10X_SIGNING_IDENTITY: `Developer ID Application: 10x Labs (${TEAM_ID})`,
    ZED_10X_NOTARIZATION_TEAM_ID: TEAM_ID,
    ZED_10X_NOTARIZATION_KEY: "test-key",
    ZED_10X_NOTARIZATION_KEY_ID: "TESTKEY123",
    ZED_10X_NOTARIZATION_ISSUER_ID: "00000000-0000-0000-0000-000000000000",
    ...overrides,
  };
}

test("verifies the fork identity and every shipped executable without --deep", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);
  const commands = [];

  const result = await verifyAppBundle({
    appPath,
    expectedCommit: COMMIT,
    expectedTeamId: TEAM_ID,
    readPlist: plistReader(),
    runCommand: successfulRunner(commands),
  });

  assert.equal(result.bundleIdentifier, "ai.10xlabs.Zed10x");
  assert.equal(result.teamId, TEAM_ID);
  assert.equal(result.sourceCommit, COMMIT);
  assert.equal(result.hardenedRuntime, true);
  assert.equal(
    commands.filter(([command, argument]) => command === "/usr/bin/codesign" && argument === "--verify").length,
    4,
  );
  assert.equal(commands.flat().includes("--deep"), false);
  assert.ok(commands.some(([command, ...args]) => command === "/usr/sbin/spctl" && args.includes("execute")));
});

test("rejects a valid signature from the wrong Developer ID team", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);

  await assert.rejects(
    verifyAppBundle({
      appPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      readPlist: plistReader(),
      runCommand: successfulRunner([], { teamId: "WRONG12345" }),
    }),
    /Developer ID team mismatch/,
  );
});

test("rejects a shipped executable signed by another Developer ID team", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);
  const runCommand = async (command, args) => {
    if (command === "/usr/bin/codesign" && args[0] === "--display") {
      const inspectedPath = args.at(-1);
      const teamId = inspectedPath.endsWith("/git") ? "WRONG12345" : TEAM_ID;
      return {
        stdout: "",
        stderr: [
          "CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=3+7 location=embedded",
          `Authority=Developer ID Application: 10x Labs (${teamId})`,
          `TeamIdentifier=${teamId}`,
        ].join("\n"),
      };
    }
    return { stdout: "", stderr: "" };
  };

  await assert.rejects(
    verifyAppBundle({
      appPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      readPlist: plistReader(),
      runCommand,
    }),
    /shipped executable Developer ID team mismatch/,
  );
});

test("rejects an inherited upstream provisioning profile", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);
  fs.writeFileSync(path.join(appPath, "Contents", "embedded.provisionprofile"), "upstream-profile");

  await assert.rejects(
    verifyAppBundle({
      appPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      readPlist: plistReader(),
      runCommand: successfulRunner([]),
    }),
    /must not embed the upstream provisioning profile/,
  );
});

test("rejects even a broken provisioning-profile symlink", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);
  fs.symlinkSync("missing-upstream-profile", path.join(appPath, "Contents", "embedded.provisionprofile"));

  await assert.rejects(
    verifyAppBundle({
      appPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      readPlist: plistReader(),
      runCommand: successfulRunner([]),
    }),
    /must not embed the upstream provisioning profile/,
  );
});

test("rejects an app without hardened runtime", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const appPath = appFixture(root);
  const commands = [];
  const runCommand = async (command, args) => {
    commands.push([command, ...args]);
    if (command === "/usr/bin/codesign" && args[0] === "--display") {
      return {
        stdout: "",
        stderr: `Authority=Developer ID Application: 10x Labs (${TEAM_ID})\nTeamIdentifier=${TEAM_ID}\nflags=0x0(none)`,
      };
    }
    return { stdout: "", stderr: "" };
  };

  await assert.rejects(
    verifyAppBundle({
      appPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      readPlist: plistReader(),
      runCommand,
    }),
    /hardened runtime/,
  );
});

test("verifies the mounted app, always detaches, and writes a content-free receipt", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const sourceApp = appFixture(root, "source.app");
  const dmgPath = path.join(root, "Zed-10x-aarch64.dmg");
  const remoteServerPath = remoteServerFixture(root);
  const receiptPath = path.join(root, "release-receipt.json");
  fs.writeFileSync(dmgPath, "signed-dmg-fixture");
  const commands = [];
  const baseRunner = successfulRunner(commands);
  const runCommand = async (command, args) => {
    if (command === "/usr/bin/hdiutil" && args[0] === "attach") {
      const mountPath = args[args.indexOf("-mountpoint") + 1];
      fs.cpSync(sourceApp, path.join(mountPath, "Zed 10x.app"), {
        recursive: true,
      });
    }
    return baseRunner(command, args);
  };

  const receipt = await verifyRelease({
    dmgPath,
    remoteServerPath,
    expectedCommit: COMMIT,
    expectedTeamId: TEAM_ID,
    receiptPath,
    readPlist: plistReader(),
    runCommand,
  });

  assert.equal(receipt.status, "verified");
  assert.equal(receipt.sourceCommit, COMMIT);
  assert.equal(receipt.teamId, TEAM_ID);
  assert.equal(receipt.dmgSha256, crypto.createHash("sha256").update("signed-dmg-fixture").digest("hex"));
  assert.equal(
    receipt.remoteServerSha256,
    crypto.createHash("sha256").update(fs.readFileSync(remoteServerPath)).digest("hex"),
  );
  assert.deepEqual(JSON.parse(fs.readFileSync(receiptPath, "utf8")), receipt);
  assert.equal(fs.statSync(receiptPath).mode & 0o777, 0o600);
  assert.ok(commands.some(([command, ...args]) => command === "/usr/sbin/spctl" && args.includes("open")));
  assert.ok(
    commands.some(
      ([command, ...args]) => command === "/usr/bin/xcrun" && args[0] === "stapler" && args[1] === "validate",
    ),
  );
  assert.ok(
    commands.some(
      ([command, ...args]) =>
        command === "/usr/bin/codesign" && args[0] === "--verify" && args.at(-1).endsWith("zed-remote-server"),
    ),
  );
  assert.ok(commands.some(([command, argument]) => command === "/usr/bin/hdiutil" && argument === "detach"));
});

test("rejects a disk image not signed with Developer ID Application", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const dmgPath = path.join(root, "Zed-10x-aarch64.dmg");
  fs.writeFileSync(dmgPath, "signed-dmg-fixture");

  await assert.rejects(
    verifyRelease({
      dmgPath,
      remoteServerPath: remoteServerFixture(root),
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      receiptPath: path.join(root, "release-receipt.json"),
      readPlist: plistReader(),
      runCommand: successfulRunner([], { authority: "Apple Development: 10x Labs" }),
    }),
    /disk image is not signed with Developer ID Application/,
  );
});

test("detaches a mounted image when app verification fails", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const sourceApp = appFixture(root, "source.app");
  const dmgPath = path.join(root, "Zed-10x-aarch64.dmg");
  const remoteServerPath = remoteServerFixture(root);
  fs.writeFileSync(dmgPath, "signed-dmg-fixture");
  const commands = [];
  const baseRunner = successfulRunner(commands);
  const runCommand = async (command, args) => {
    if (command === "/usr/bin/hdiutil" && args[0] === "attach") {
      const mountPath = args[args.indexOf("-mountpoint") + 1];
      fs.cpSync(sourceApp, path.join(mountPath, "Zed 10x.app"), {
        recursive: true,
      });
    }
    return baseRunner(command, args);
  };

  await assert.rejects(
    verifyRelease({
      dmgPath,
      remoteServerPath,
      expectedCommit: COMMIT,
      expectedTeamId: TEAM_ID,
      receiptPath: path.join(root, "release-receipt.json"),
      readPlist: plistReader({ ":CFBundleIdentifier": "dev.zed.Zed" }),
      runCommand,
    }),
    /bundle identifier mismatch/,
  );
  assert.ok(commands.some(([command, argument]) => command === "/usr/bin/hdiutil" && argument === "detach"));
});

test("force-detaches a mounted image when ordinary detach fails", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const sourceApp = appFixture(root, "source.app");
  const dmgPath = path.join(root, "Zed-10x-aarch64.dmg");
  const remoteServerPath = remoteServerFixture(root);
  fs.writeFileSync(dmgPath, "signed-dmg-fixture");
  const commands = [];
  const baseRunner = successfulRunner(commands);
  const runCommand = async (command, args) => {
    if (command === "/usr/bin/hdiutil" && args[0] === "attach") {
      const mountPath = args[args.indexOf("-mountpoint") + 1];
      fs.cpSync(sourceApp, path.join(mountPath, "Zed 10x.app"), {
        recursive: true,
      });
    }
    if (command === "/usr/bin/hdiutil" && args[0] === "detach" && args[1] !== "-force") {
      commands.push([command, ...args]);
      throw new Error("busy mount");
    }
    return baseRunner(command, args);
  };

  await verifyRelease({
    dmgPath,
    remoteServerPath,
    expectedCommit: COMMIT,
    expectedTeamId: TEAM_ID,
    receiptPath: path.join(root, "release-receipt.json"),
    readPlist: plistReader(),
    runCommand,
  });

  assert.ok(
    commands.some(
      ([command, operation, option]) => command === "/usr/bin/hdiutil" && operation === "detach" && option === "-force",
    ),
  );
});

test("never replaces an existing immutable release receipt", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "zed-10x-release-test-"));
  const sourceApp = appFixture(root, "source.app");
  const dmgPath = path.join(root, "Zed-10x-aarch64.dmg");
  const remoteServerPath = remoteServerFixture(root);
  const receiptPath = path.join(root, "release-receipt.json");
  fs.writeFileSync(dmgPath, "signed-dmg-fixture");
  const baseRunner = successfulRunner([]);
  const runCommand = async (command, args) => {
    if (command === "/usr/bin/hdiutil" && args[0] === "attach") {
      const mountPath = args[args.indexOf("-mountpoint") + 1];
      fs.cpSync(sourceApp, path.join(mountPath, "Zed 10x.app"), {
        recursive: true,
      });
    }
    return baseRunner(command, args);
  };
  const options = {
    dmgPath,
    remoteServerPath,
    expectedCommit: COMMIT,
    expectedTeamId: TEAM_ID,
    receiptPath,
    readPlist: plistReader(),
    runCommand,
  };

  await verifyRelease(options);
  const firstReceipt = fs.readFileSync(receiptPath);
  await assert.rejects(verifyRelease(options), /EEXIST/);
  assert.deepEqual(fs.readFileSync(receiptPath), firstReceipt);
});

test("bundle-mac keeps fork release signing isolated from the login keychain", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const bundleScript = fs.readFileSync(path.join(repositoryRoot, "script", "bundle-mac"), "utf8");
  const signingScript = fs.readFileSync(path.join(repositoryRoot, "script", "sign-zed-10x-macos-release"), "utf8");

  assert.match(bundleScript, /--zed-10x-prepare-release/);
  assert.doesNotMatch(bundleScript, /ZED_10X_MACOS_CERTIFICATE/);
  assert.match(signingScript, /ZED_10X_SIGNING_IDENTITY/);
  assert.match(signingScript, /ZED_10X_NOTARIZATION_TEAM_ID/);
  assert.doesNotMatch(bundleScript, /security default-keychain -s/);
  assert.doesNotMatch(signingScript, /security default-keychain -s/);
  assert.match(signingScript, /verify-zed-10x-macos-release\.mjs/);
  assert.match(signingScript, /notarization_timeout=\$\{ZED_10X_NOTARIZATION_TIMEOUT:-60m\}/);
  assert.match(signingScript, /--timeout "\$notarization_timeout"/);
  assert.match(bundleScript, /2be2669972dff3ddd4daf89a2cb29d2d06cad7c7/);
  assert.match(bundleScript, /cargo install[\s\S]*--locked[\s\S]*--rev "\$cargo_bundle_revision"/);
  assert.doesNotMatch(bundleScript, /cargo install cargo-bundle[^\n]*--branch/);
  assert.match(bundleScript, /f014b290f36d121bbc47b29b556044c4f6a2f6494cf78ed2eacfa501b77363b6/);
  assert.match(bundleScript, /02fb29a47a07e9a7f841888661f32ebc80a56930864dd19b674d80ffc429c96d/);
  assert.doesNotMatch(bundleScript, /curl[^\n]*\|\s*tar/);
});

test("release credentials are visible only to the signing step", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const workflow = fs.readFileSync(path.join(repositoryRoot, ".github", "workflows", "zed-10x-release.yml"), "utf8");
  const buildStepOffset = workflow.indexOf("- name: Build the unsigned release inputs");
  const signingStepOffset = workflow.indexOf("- name: Sign, notarize, and verify");
  const uploadStepOffset = workflow.indexOf("- name: Retain the verified disk image and receipt");

  assert.ok(buildStepOffset > 0);
  assert.ok(signingStepOffset > buildStepOffset);
  assert.ok(uploadStepOffset > signingStepOffset);
  assert.doesNotMatch(workflow.slice(0, signingStepOffset), /secrets\./);
  assert.match(workflow.slice(signingStepOffset, uploadStepOffset), /secrets\.ZED_10X_MACOS_CERTIFICATE/);
  assert.doesNotMatch(workflow.slice(uploadStepOffset), /secrets\./);
  assert.match(workflow, /github\.ref == 'refs\/heads\/main'/);
  assert.match(workflow, /environment: zed-10x-release/);
});

test("release signer fails before touching artifacts when signing authority is absent", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const result = spawnSync("/bin/bash", ["script/sign-zed-10x-macos-release", "aarch64-apple-darwin"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: {
      HOME: os.homedir(),
      PATH: "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
    },
  });

  assert.equal(result.status, 2);
  assert.match(result.stderr, /Missing required Zed 10x release variable/);
});

test("release signer rejects an unbounded or malformed notarization wait", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const result = spawnSync("/bin/bash", ["script/sign-zed-10x-macos-release", "aarch64-apple-darwin"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: fakeReleaseEnvironment({ ZED_10X_NOTARIZATION_TIMEOUT: "forever" }),
  });

  assert.equal(result.status, 2);
  assert.match(result.stderr, /must be a positive notarytool duration/);
});

test("release signer rejects a signing identity from another team", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const result = spawnSync("/bin/bash", ["script/sign-zed-10x-macos-release", "aarch64-apple-darwin"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: fakeReleaseEnvironment({
      ZED_10X_SIGNING_IDENTITY: "Developer ID Application: 10x Labs (WRONG12345)",
    }),
  });

  assert.equal(result.status, 2);
  assert.match(result.stderr, /must be the full Developer ID Application identity/);
});

test("release signer unsets reusable credentials before any build-capable subprocess", () => {
  const repositoryRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
  const signingScript = fs.readFileSync(path.join(repositoryRoot, "script", "sign-zed-10x-macos-release"), "utf8");
  const unsetOffset = signingScript.indexOf("unset \\");
  const firstExternalToolOffset = signingScript.indexOf("signing_directory=$(mktemp -d)");

  assert.ok(unsetOffset > 0);
  assert.ok(firstExternalToolOffset > unsetOffset);
  assert.doesNotMatch(signingScript, /cargo (?:build|install)|npm install|curl /);
});

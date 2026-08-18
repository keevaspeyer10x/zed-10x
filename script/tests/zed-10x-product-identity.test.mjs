import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../..",
);

function read(relativePath) {
  return fs.readFileSync(path.join(repositoryRoot, relativePath), "utf8");
}

function capture(source, pattern, label) {
  const match = source.match(pattern);
  assert.ok(match, `${label} declaration must exist`);
  return match[1];
}

test("compiled paths and remote server storage use the Zed 10x identity", () => {
  const pathsSource = read("crates/paths/src/paths.rs");

  assert.equal(
    capture(
      pathsSource,
      /pub const APP_NAME: &str = "([^"]+)";/,
      "application name",
    ),
    "Zed-10x",
  );
  assert.equal(
    capture(
      pathsSource,
      /remote_server_dir_relative[\s\S]*?RelPath::from_unix_str\("([^"]+)"\)/,
      "remote server directory",
    ),
    ".zed-10x-server",
  );
});

test("the development channel has a non-Zed application identity", () => {
  const releaseChannelSource = read("crates/release_channel/src/lib.rs");

  assert.equal(
    capture(
      releaseChannelSource,
      /pub fn display_name[\s\S]*?ReleaseChannel::Dev => "([^"]+)"/,
      "development display name",
    ),
    "Zed 10x",
  );
  assert.equal(
    capture(
      releaseChannelSource,
      /pub fn app_id\(&self\)[\s\S]*?ReleaseChannel::Dev => "([^"]+)"/,
      "development application identifier",
    ),
    "ai.10xlabs.Zed10x",
  );
  assert.match(
    releaseChannelSource,
    /ReleaseChannel::Dev\s*=>\s*cfg!\(target_os = "macos"\)\s*&&\s*!cfg!\(debug_assertions\)/,
    "only signed release-mode macOS builds may poll the fork update channel",
  );
});

test("Zed 10x updates use consumer-safe verification and commit-specific remote servers", () => {
  const updaterSource = read("crates/auto_update/src/auto_update.rs");
  const sshSource = read("crates/remote/src/transport/ssh.rs");
  const dockerSource = read("crates/remote/src/transport/docker.rs");

  assert.match(updaterSource, /\("Zed 10x", OsStr::new\("Zed 10x\.app"\)\)/);
  assert.doesNotMatch(updaterSource, /new_command\("\/usr\/bin\/xcrun"\)/);
  assert.match(updaterSource, /libc::RENAME_SWAP/);
  assert.doesNotMatch(sshSource, /ReleaseChannel::Dev\s*=>\s*"build"/);
  assert.doesNotMatch(dockerSource, /ReleaseChannel::Dev\s*=>\s*"build"/);
  assert.match(sshSource, /ReleaseChannel::Dev\s*=>\s*Some\(version\.clone\(\)\)/);
  assert.match(dockerSource, /ReleaseChannel::Dev\s*=>\s*Some\(version\.clone\(\)\)/);
});

test("the macOS bundle and executable are independently addressable", () => {
  const cargoManifest = read("crates/zed/Cargo.toml");
  const bundleScript = read("script/bundle-mac");

  assert.equal(
    capture(cargoManifest, /default-run = "([^"]+)"/, "default binary"),
    "zed-10x",
  );
  assert.equal(
    capture(
      cargoManifest,
      /\[\[bin\]\]\s+name = "([^"]+)"\s+path = "src\/main\.rs"/,
      "main binary",
    ),
    "zed-10x",
  );

  const developmentBundle = cargoManifest.match(
    /\[package\.metadata\.bundle-dev\]([\s\S]*?)(?=\n\[|$)/,
  )?.[1];
  assert.ok(developmentBundle, "development bundle metadata must exist");
  assert.equal(
    capture(developmentBundle, /identifier = "([^"]+)"/, "bundle identifier"),
    "ai.10xlabs.Zed10x",
  );
  assert.equal(
    capture(developmentBundle, /name = "([^"]+)"/, "bundle name"),
    "Zed 10x",
  );
  assert.equal(
    capture(
      developmentBundle,
      /osx_url_schemes = \["([^"]+)"\]/,
      "URL scheme",
    ),
    "zed-10x",
  );

  assert.match(
    bundleScript,
    /target\/\$\{target_triple\}\/\$\{target_dir\}\/zed-10x/,
  );
  assert.doesNotMatch(
    bundleScript,
    /target\/\$\{target_triple\}\/\$\{target_dir\}\/zed(?:["'\s]|$)/,
  );
});

test("macOS bundling is transaction-safe in headless shells", () => {
  const bundleScript = read("script/bundle-mac");

  assert.ok(
    /restore_bundle_manifest\(\)[\s\S]*?mv Cargo\.toml\.backup Cargo\.toml/.test(
      bundleScript,
    ),
    "the temporary bundle manifest must have an exit-safe restore function",
  );
  assert.ok(
    /trap restore_bundle_manifest EXIT/.test(bundleScript),
    "the manifest restore function must be registered before bundling",
  );
  assert.ok(
    /TERM=xterm-256color cargo bundle \$\{build_flag\}/.test(bundleScript),
    "cargo-bundle must receive a colour-capable terminal in headless shells",
  );
  assert.ok(
    /restore_bundle_manifest\s+trap - EXIT/.test(bundleScript),
    "successful bundling must restore the manifest and clear the temporary trap",
  );
  assert.ok(
    !/target\/\$\{?target_triple\}?\/release\/remote_server/.test(bundleScript),
    "remote-server packaging must not hard-code the release build directory",
  );
  assert.ok(
    /sign_binary "target\/\$\{target_triple\}\/\$\{target_dir\}\/remote_server"/.test(
      bundleScript,
    ),
    "remote-server signing must use the selected build directory",
  );
  assert.ok(
    /gzip[\s\S]*?target\/\$\{target_triple\}\/\$\{target_dir\}\/remote_server/.test(
      bundleScript,
    ),
    "remote-server compression must use the selected build directory",
  );
});

test("the development app bundle owns the canary launcher assembly", () => {
  const bundleScript = read("script/bundle-mac");
  const assembly = bundleScript.match(
    /function assemble_zed_10x_canary\(\) \{([\s\S]*?)\n\}/,
  )?.[1];
  assert.ok(assembly, "the bundle script must define canary assembly");
  const callSites = [
    ...bundleScript.matchAll(/^\s*assemble_zed_10x_canary\s*$/gm),
  ];
  assert.equal(
    callSites.length,
    1,
    "the bundle script must invoke canary assembly exactly once",
  );

  assert.match(
    assembly,
    /if \[\[ "\$channel" == "dev" \]\]; then[\s\S]*?Contents\/MacOS\/zed-10x-runtime/,
    "development bundling must keep the real executable beside its macOS helpers",
  );
  assert.match(
    assembly,
    /Contents\/MacOS\/zed-10x-launcher[\s\S]*?install -m 0755 script\/zed-10x-canary-launcher/,
    "the launcher must be copied into the app bundle",
  );
  assert.match(
    assembly,
    /Contents\/Resources\/zed-10x-canary\.mjs[\s\S]*?install -m 0755 script\/zed-10x-canary\.mjs/,
    "the fail-open collector must be copied into the app bundle",
  );
  assert.match(
    assembly,
    /Contents\/Resources\/zed-10x-git-commit[\s\S]*?git rev-parse HEAD/,
    "the exact source revision must be recorded for telemetry provenance",
  );
  assert.match(
    assembly,
    /CFBundleExecutable[\s\S]*?zed-10x-launcher/,
    "LaunchServices must enter through the telemetry launcher",
  );
});

test("the packaged CLI follows the Zed 10x launcher contract and fails finitely", () => {
  const bundleScript = read("script/bundle-mac");
  const cliSource = read("crates/cli/src/main.rs");

  assert.match(
    bundleScript,
    /ZedCliLaunchExecutableDirectly[\s\S]*?bool true/,
    "the Zed 10x bundle must explicitly opt its CLI into the proven direct launcher path",
  );
  assert.match(
    cliSource,
    /CFBundleExecutable/,
    "the packaged CLI must discover the executable declared by Info.plist",
  );
  assert.match(
    cliSource,
    /ZedCliLaunchExecutableDirectly/,
    "the direct-launch exception must be an explicit bundle contract",
  );
  assert.match(
    cliSource,
    /wait_for_app_handshake\([\s\S]*?APP_HANDSHAKE_TIMEOUT\)/,
    "the app-to-CLI handshake must have a finite deadline",
  );
  assert.doesNotMatch(
    cliSource,
    /app_bundle\.join\("Contents\/MacOS\/zed"\)/,
    "the CLI must not assume that every macOS product binary is named zed",
  );
});

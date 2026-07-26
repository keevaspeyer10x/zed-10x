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

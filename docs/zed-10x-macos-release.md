# Zed 10x macOS release and updates

Zed 10x uses a fork-owned macOS release path. It does not reuse Zed Industries'
application identity, signing identity, provisioning profile, or update state.

The `Zed 10x macOS Release Artifact` workflow is intentionally manual and runs
only from protected `main`. A credential-free step builds the selected commit.
The unsigned inputs cross a pinned artifact boundary into a fresh runner. A
separate signer then signs the application with a 10x Labs Developer ID
certificate, submits the disk image to Apple's notarization service, staples
the ticket, verifies the mounted artifact and signed remote server, and retains
both artifacts with a content-free verification receipt. A third job rebinds
those files to the receipt, creates a draft GitHub release, uploads the exact
assets, verifies the complete uploaded draft, and publishes it as the current
Zed 10x update candidate. The same workflow also builds and publishes
commit-matched Linux x86_64 and aarch64 remote servers for remote connections.

## Required GitHub Actions secrets

Configure these secrets in the protected `zed-10x-release` GitHub Environment
before dispatching the workflow:

- `ZED_10X_MACOS_CERTIFICATE`: base64-encoded Developer ID Application PKCS#12
  certificate and private key.
- `ZED_10X_MACOS_CERTIFICATE_PASSWORD`: PKCS#12 password.
- `ZED_10X_SIGNING_IDENTITY`: full `Developer ID Application: ... (TEAMID)`
  identity.
- `ZED_10X_NOTARIZATION_TEAM_ID`: ten-character Apple Developer team ID.
- `ZED_10X_NOTARIZATION_KEY`: App Store Connect API private-key contents.
- `ZED_10X_NOTARIZATION_KEY_ID`: App Store Connect API key ID.
- `ZED_10X_NOTARIZATION_ISSUER_ID`: App Store Connect API issuer ID.

The workflow exposes none of these values to the build job or as artifacts.
`script/sign-zed-10x-macos-release` copies the credentials into private shell
state, unsets the inherited environment before invoking any external tool,
imports the certificate into a private temporary keychain without changing the
runner's default keychain, and removes the temporary key material on exit. The
credential-free preparation mode installs `cargo-bundle` from the archived Zed
fork at a fixed Git revision with its checked-in lockfile.

## Verification contract

`script/verify-zed-10x-macos-release.mjs` fails unless all of the following are
true:

- the disk image and application pass strict code-signature verification;
- Gatekeeper accepts both the disk image and application;
- Apple stapler validates the notarization ticket;
- the application carries the `ai.10xlabs.Zed10x` identity, `zed-10x` URL
  scheme, expected source commit, and expected Developer ID team;
- the bundle does not contain Zed Industries' embedded provisioning profile;
- the application signature enables hardened runtime;
- the disk image contains exactly one `Zed 10x.app` bundle; and
- every shipped Mach-O executable has a valid signature.
- the separately compressed remote server expands to a signed, hardened-runtime
  executable from the same Developer ID team.

The verifier writes an immutable JSON receipt beside the disk image. That
receipt includes only artifact identity and hashes; it contains no signing or
notarization credentials.

Apple notarization gets a 60-minute wait by default. Set
`ZED_10X_NOTARIZATION_TIMEOUT` to another positive `notarytool` duration when a
longer service window is justified. A timeout or rejection produces no verified
receipt and therefore no releasable artifact.

## Update and rollback boundary

Repository release immutability must be enabled. Release builds on the Zed 10x
development channel poll the repository's public GitHub Releases API and reject
draft, prerelease, mutable, digest-free, ambiguous, or wrong-origin metadata.
The updater verifies the immutable SHA-256 asset digest, Developer ID team,
Gatekeeper notarization assessment, bundle identifier, and complete app
signature before installation. Release engineering separately validates the
stapled ticket before publication, so updater clients do not require Xcode or
Command Line Tools.

Installation stages the verified app at the rollback path on the destination
volume and atomically exchanges it with the running bundle in one filesystem
operation. The immediately previous verified bundle remains beside
the installed app as `.Zed 10x.app.previous`. With Zed 10x stopped, restore it
with:

```sh
./script/rollback-zed-10x-macos "/Applications/Zed 10x.app"
```

The rollback command verifies both app identities, signatures, Gatekeeper
assessments, and Developer ID teams before swapping them. It never touches
upstream Zed's bundle, profile, database, or update channel.

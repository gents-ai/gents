# macOS release signing

Deployed macOS steward agents must be installed from the signed `gents`
release artifact produced by `.github/workflows/release-macos.yml`.

## Why ad-hoc signing is not enough

The steward agent reads secrets from the user's login keychain while running
under `launchd`. Keychain access control is tied to the code identity that first
used or was approved for the item. For an ad-hoc signed binary, that identity is
effectively tied to the binary's code hash. Every rebuild changes the hash.

That makes rollout non-repeatable: `launchd` can start the replacement binary,
but the process may block before HTTP startup when it reaches
`SecKeychainFindGenericPassword`, because the existing keychain item still trusts
the old ad-hoc hash.

CI release artifacts are signed with a stable certificate identity and the
explicit code identifier:

```text
com.source-inc.gents.cli
```

Future releases signed with the same certificate and identifier share a stable
designated requirement, so keychain trust can survive binary updates.

## Release workflow

The macOS release workflow:

- runs on a self-hosted Mac Studio runner labeled `self-hosted`, `macOS`,
  `ARM64`, and `studio`;
- caps Cargo build fan-out with `CARGO_BUILD_JOBS` and uses disk-backed
  `sccache` to reduce repeated Rust compilation on memory-constrained Studio
  runners;
- builds `gents-cli` in release mode, producing `target/release/gents`;
- unlocks the runner's persistent signing keychain;
- makes that keychain visible to non-interactive `codesign` jobs;
- smoke-signs a test binary before the Rust build starts;
- signs with `--identifier com.source-inc.gents.cli`;
- submits the signed CLI to Apple's notary service;
- verifies with `codesign --verify --strict --verbose=2`;
- prints the designated requirement with `codesign -d -r-`;
- prints `spctl --assess --type execute` diagnostics;
- runs `gents version` so a signed-but-not-launchable binary fails before
  packaging;
- packages `gents-aarch64-apple-darwin.tar.gz`;
- writes a `.sha256` checksum file;
- uploads both files as workflow artifacts;
- attaches both files to the GitHub Release for tags matching `v*`.

Tagged releases require signing secrets and will fail rather than publish an
unsigned artifact. A manual `workflow_dispatch` run can set `dry_run_unsigned`
for build-only validation, but the artifact is named `unsigned-dry-run` and must
not be deployed.

The Studio runners should have Homebrew protobuf available at
`/opt/homebrew/bin/protoc`. The workflow sets `PROTOC` explicitly from that path
when present, because non-interactive runner shells may not include
`/opt/homebrew/bin` in `PATH`.

## Required GitHub secrets

Set these repository or environment secrets before running the release workflow:

```text
MACOS_CODESIGN_IDENTITY
MACOS_CODESIGN_KEYCHAIN_PASSWORD
MACOS_NOTARY_API_KEY
MACOS_NOTARY_ISSUER_ID
MACOS_NOTARY_KEY_ID
```

`MACOS_CODESIGN_IDENTITY` can be the identity name shown by
`security find-identity -v -p codesigning`, or the certificate SHA-1 hash from
that output.

`MACOS_CODESIGN_KEYCHAIN_PASSWORD` must unlock the persistent signing keychain on
each Studio runner.

`MACOS_NOTARY_API_KEY` must contain the App Store Connect `.p8` key contents.
`MACOS_NOTARY_KEY_ID` and `MACOS_NOTARY_ISSUER_ID` are passed to
`xcrun notarytool submit --key-id ... --issuer ...`.

The workflow also honors optional repository variables:

```text
CARGO_BUILD_JOBS=4
SCCACHE_CACHE_SIZE=60G
SCCACHE_DIR=/Users/admin/.cache/sccache
MACOS_CODESIGN_KEYCHAIN_PATH=/Users/admin/Library/Keychains/gents-signing.keychain-db
MACOS_CODESIGN_TIMESTAMP_MODE=auto|enabled
```

`CARGO_BUILD_JOBS` defaults to `4` to keep Rust compilation from exhausting the
non-model memory headroom on shared Studio hosts. `SCCACHE_DIR` defaults to a
disk-backed cache under `/Users/admin/.cache/sccache`; do not point it at a
memory-backed volume.

`MACOS_CODESIGN_KEYCHAIN_PATH` defaults to
`$HOME/Library/Keychains/gents-signing.keychain-db`.

Signed release artifacts must use a timestamped Developer ID signature because
the workflow notarizes them. The default `auto` mode uses `--timestamp` and
fails if timestamping is unavailable. `none` is rejected for signed releases and
is only meaningful for unsigned manual dry-run builds where signing is skipped.

Raw command-line binaries are not a stapler-supported file format, and
`spctl --assess --type execute` can reject them with "the code is valid but does
not seem to be an app". The workflow still notarizes a zip containing the signed
binary, prints `spctl` diagnostics, and gates the release on an executable
launch smoke. Offline stapling would require changing the release artifact to a
signed flat package, disk image, or app bundle.

## Studio signing keychain

Production path: use an Apple Developer ID Application certificate owned by
Source Network. Tagged release artifacts must be notarized, so self-signed
identities are no longer a valid release path. Use `dry_run_unsigned` only for
manual build validation artifacts that will not be deployed.

Each self-hosted Studio runner must have the signing identity installed in a
persistent keychain that CI can unlock over SSH and from the GitHub Actions
runner session. The release workflow defaults to:

```text
/Users/admin/Library/Keychains/gents-signing.keychain-db
```

The current release identity is a Developer ID Application identity. The workflow
finds it in the signing keychain, refreshes private-key access for `codesign`,
puts the keychain first in the active search list, smoke-signs a copy of
`/bin/echo`, and refuses to continue if the designated requirement is an ad-hoc
`cdhash`.

The Gents Studio runner is launched by
`/Library/LaunchDaemons/com.github.actions.runner.gents.plist`. That
LaunchDaemon must set `SessionCreate=true`; otherwise the job can update the
user keychain search list while the default keychain domain seen by `codesign`
still contains only the System keychain. Use:

```sh
scripts/enable-gents-runner-session.sh
```

To create or rotate the identity on a Studio:

1. Open Keychain Access.
2. Import the Apple-issued Developer ID Application certificate and private key
   into the login keychain.
3. In Keychain Access, export the signing identity as a `.p12` file and set an
   export password.
4. Create or unlock the persistent CI signing keychain.
5. Import the `.p12` into that keychain.
6. Allow `codesign` and `security` to use the private key from non-interactive
   runner jobs.
7. Store the keychain password in `MACOS_CODESIGN_KEYCHAIN_PASSWORD`.
8. Find the identity value:

```sh
security find-identity -v -p codesigning
```

9. Store the identity name or SHA-1 hash in `MACOS_CODESIGN_IDENTITY`.
10. Create or rotate an App Store Connect API key with notarization access and
    store it in `MACOS_NOTARY_API_KEY`, `MACOS_NOTARY_KEY_ID`, and
    `MACOS_NOTARY_ISSUER_ID`.

The exported `.p12` is only needed for runner provisioning or certificate
rotation. The release workflow does not import a `.p12` at runtime; tagged
release artifacts must be signed by the native Studio keychain path.

## Local verification

After downloading the workflow or release artifacts:

```sh
shasum -a 256 -c gents-aarch64-apple-darwin.tar.gz.sha256
tar -xzf gents-aarch64-apple-darwin.tar.gz
codesign --verify --strict --verbose=2 gents-aarch64-apple-darwin/gents
codesign -d -r- gents-aarch64-apple-darwin/gents
codesign -d -vvv gents-aarch64-apple-darwin/gents
spctl --assess --type execute --verbose=4 gents-aarch64-apple-darwin/gents || true
./gents-aarch64-apple-darwin/gents version
```

## Gents rollout

Use the [Gents cutover runbook](gents-cutover.md) for deployed hosts. Gents does
not migrate identities or keychain items from a pre-Gents installation. Install
the signed artifact, initialize fresh Gents state, and let the runtime create a
new identity under the `com.source-inc.gents.identity` keychain service.

Pre-cutover state is left untouched. Remove it manually only after the new DID,
peer relationships, runtime health, and rollback records have been verified.
Future steward updates should use only the signed release artifact so the
keychain sees the same signing requirement across Gents releases.

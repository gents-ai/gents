# macOS release signing

Deployed macOS steward agents must be installed from the signed `defra-agent`
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
org.sourcenetwork.defra-agent
```

Future releases signed with the same certificate and identifier share a stable
designated requirement, so keychain trust can survive binary updates.

## Release workflow

The macOS release workflow:

- runs on a self-hosted Mac Studio runner labeled `self-hosted`, `macOS`,
  `ARM64`, and `studio`;
- builds `defra-agent-cli` in release mode, producing `target/release/defra-agent`;
- unlocks the runner's persistent signing keychain;
- makes that keychain visible to non-interactive `codesign` jobs;
- smoke-signs a test binary before the Rust build starts;
- signs with `--identifier org.sourcenetwork.defra-agent`;
- verifies with `codesign --verify --strict --verbose=2`;
- prints the designated requirement with `codesign -d -r-`;
- packages `defra-agent-aarch64-apple-darwin.tar.gz`;
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
PRIVATE_REPO_PAT
MACOS_CODESIGN_IDENTITY
MACOS_CODESIGN_KEYCHAIN_PASSWORD
```

`PRIVATE_REPO_PAT` is the same private dependency fetch token used by CI. The
workflow disables checkout's persisted `GITHUB_TOKEN` credentials and uses this
PAT for Source Network git dependencies, matching the Backbone CI pattern.

`MACOS_CODESIGN_IDENTITY` can be the identity name shown by
`security find-identity -v -p codesigning`, or the certificate SHA-1 hash from
that output.

`MACOS_CODESIGN_KEYCHAIN_PASSWORD` must unlock the persistent signing keychain on
each Studio runner.

The workflow also honors optional repository variables:

```text
MACOS_CODESIGN_KEYCHAIN_PATH=/Users/admin/Library/Keychains/defra-agent-signing.keychain-db
MACOS_CODESIGN_TIMESTAMP_MODE=auto|enabled|none
MACOS_CODESIGN_SPCTL_REQUIRED=true|false
```

`MACOS_CODESIGN_KEYCHAIN_PATH` defaults to
`$HOME/Library/Keychains/defra-agent-signing.keychain-db`.

Use `enabled` for Developer ID release signing when timestamping must succeed.
Use `none` for self-signed or internal identities that cannot use Apple's
timestamp service. The default `auto` mode tries `--timestamp` first and retries
with `--timestamp=none` if timestamping is unavailable.

`spctl` assessment is non-blocking by default because self-signed identities do
not pass Gatekeeper assessment. Set `MACOS_CODESIGN_SPCTL_REQUIRED=true` only for
Developer ID releases where Gatekeeper assessment is expected to pass.

## Studio signing keychain

Preferred production path: use an Apple Developer ID Application certificate
owned by Source Network.

Internal path: use a stable self-signed code-signing certificate that is kept and
reused for all steward releases. Replacing the certificate changes the
designated requirement and may require another keychain approval/migration.

Each self-hosted Studio runner must have the signing identity installed in a
persistent keychain that CI can unlock over SSH and from the GitHub Actions
runner session. The release workflow defaults to:

```text
/Users/admin/Library/Keychains/defra-agent-signing.keychain-db
```

The current release identity is a Developer ID Application identity. The workflow
finds it in the signing keychain, refreshes private-key access for `codesign`,
puts the keychain first in the active search list, smoke-signs a copy of
`/bin/echo`, and refuses to continue if the designated requirement is an ad-hoc
`cdhash`.

To create or rotate the identity on a Studio:

1. Open Keychain Access.
2. For Developer ID, import the Apple-issued certificate and private key into the
   login keychain.
3. For an internal identity, use Certificate Assistant to create a self-signed
   code-signing certificate in the login keychain.
4. In Keychain Access, export the signing identity as a `.p12` file and set an
   export password.
5. Create or unlock the persistent CI signing keychain.
6. Import the `.p12` into that keychain.
7. Allow `codesign` and `security` to use the private key from non-interactive
   runner jobs.
8. Store the keychain password in `MACOS_CODESIGN_KEYCHAIN_PASSWORD`.
9. Find the identity value:

```sh
security find-identity -v -p codesigning
```

10. Store the identity name or SHA-1 hash in `MACOS_CODESIGN_IDENTITY`.

The exported `.p12` is only needed for runner provisioning or certificate
rotation. The release workflow does not import a `.p12` at runtime; tagged
release artifacts must be signed by the native Studio keychain path.

## Local verification

After downloading the workflow or release artifacts:

```sh
shasum -a 256 -c defra-agent-aarch64-apple-darwin.tar.gz.sha256
tar -xzf defra-agent-aarch64-apple-darwin.tar.gz
codesign --verify --strict --verbose=2 defra-agent-aarch64-apple-darwin/defra-agent
codesign -d -r- defra-agent-aarch64-apple-darwin/defra-agent
codesign -d -vvv defra-agent-aarch64-apple-darwin/defra-agent
```

For Developer ID artifacts, Gatekeeper assessment should also pass:

```sh
spctl --assess --type execute --verbose=4 defra-agent-aarch64-apple-darwin/defra-agent
```

For self-signed/internal artifacts, `spctl` can fail even when the binary is
properly signed for stable keychain identity.

## Rollout note

Hosts that previously ran ad-hoc signed steward binaries may still have keychain
items whose access control trusts the old ad-hoc code hash. The first rollout to
the stable signed artifact can require one-time user approval, keychain ACL
migration, or deleting and recreating the item while running the stable signed
binary.

After that migration, future deployed steward binaries should come only from the
signed release artifact path so the keychain sees the same signing requirement
across releases.

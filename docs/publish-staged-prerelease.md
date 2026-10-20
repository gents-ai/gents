# Publish a staged macOS prerelease

Workflow: `.github/workflows/publish-staged-prerelease.yml`
Script: `.github/scripts/publish_staged_prerelease.py collect|publish`
Tests: `python3 .github/scripts/test-publish-staged-prerelease.py`. All GitHub
calls go through a fake `gh`. The static workflow checks need PyYAML and are
skipped without it. The tests are not wired into `ci.yml`; run them manually.

This workflow publishes a **new** macOS prerelease (`vX.Y.Z-rcN`) from the
outputs of two successful `notarize-staged-artifact.yml` runs: one for the
desktop app and one for the CLI. It never builds, never signs, and never
modifies an existing tag or release.

## Why the release rebuild workflows do not start

Pushing a `v*` tag normally starts `release-macos`,
`release-desktop-packages`, `release-linux` and `release-linux-desktop`.
Those workflows rebuild and upload assets, and `release-macos` uploads with
`--clobber`. This workflow does not push a tag. It creates a draft release
targeting the exact commit and then publishes it, and publishing creates the
tag. All of this uses the workflow's `GITHUB_TOKEN`. GitHub does not start
new workflow runs for events created by `GITHUB_TOKEN`, except
`workflow_dispatch` and `repository_dispatch`. No repository settings and no
other credentials are involved.

After publishing, the script queries push-event runs for the tag. If any
exist, it fails loudly so they can be cancelled before they change assets.
This check is the evidence that the behavior held; the documented GitHub
behavior is not taken on trust.

## What `collect` verifies (read-only)

- **Inputs.**
  - The tag must match `vX.Y.Z-rcN`.
  - The target must be a full 40-hex SHA.
  - The run ids must be numeric and different from each other.
  - The pinned hashes must be 64-hex.
  - The provenance note must have no control characters.
- **Target.** The commit exists. The tag does not exist, and no release or
  draft exists for it. The release listing is paged, capped, and fails closed
  if malformed.
- **Each notarize run.**
  - It is a `workflow_dispatch` run of `notarize-staged-artifact.yml` on the
    default branch, in this repository, `completed` / `success`.
  - Its output artifact is exactly one, unexpired, named
    `notarize-staged-<run>-<attempt>`.
  - Every entry in that artifact is a plain file name.
- **Apple acceptance, re-checked offline.**
  - The receipt has a valid submission id and a staged SHA-256.
  - `notary-log.json` must have `jobId` equal to the receipt id, `status`
    `Accepted`, and `sha256` equal to the staged SHA-256.
- **Output kind and pinned hashes.**
  - `SHA256SUMS` must be exactly one line and must match the file.
  - The app run must produce a `*-notarized.zip`; the CLI run must produce
    `gents`.
  - Each must equal the operator-pinned `app_asset_sha256` /
    `cli_binary_sha256`.
- **Assets built.**
  - `gents-desktop_<version>_aarch64.app.zip`: the notarized, stapled app zip,
    byte-for-byte.
  - `gents-aarch64-apple-darwin.tar.gz`: a deterministic tarball (fixed
    order, uid/gid 0, commit timestamp, gzip mtime 0) with the same layout as
    earlier releases, `gents-aarch64-apple-darwin/{LICENSE,gents}`. `LICENSE`
    is taken from the target commit. The tarball is re-read to confirm it
    contains the pinned binary.
  - `SHA256SUMS-desktop-macos.txt` and `SHA256SUMS-cli-macos.txt`.
  - Release notes stating the exact scope: macOS arm64 app and CLI only, no
    DMG, no Linux, no x86_64. The notes also list the submission ids, the
    hashes, the source commit, and the optional provenance note.
- **Evidence upload.** The assets, notes and manifest are uploaded as a
  workflow artifact.

## What `publish` does

1. Re-hashes the local assets against the manifest, and re-checks that the
   tag and release are still absent.
2. Runs `gh release create --draft --prerelease --latest=false --target
   <sha>` with the assets.
3. Requires exactly one draft for the tag with the right target and the
   prerelease flag. Its asset names must equal the manifest exactly, and each
   GitHub-computed `digest` must equal the local SHA-256. On any mismatch the
   release is **left as a draft** and the step fails.
4. Publishes by PATCHing `draft: false`, then checks that the tag ref is a
   commit ref pointing at the target SHA.
5. Waits briefly, then requires zero push-triggered runs for the tag.

## Operator procedure

1. Finish both `notarize-staged-artifact.yml` runs (app and CLI). Note their
   run ids, the app zip SHA-256 from the app run's `SHA256SUMS`, and the CLI
   binary SHA-256. Check the CLI SHA-256 against the build receipt.
2. Dispatch from the default branch:

   ```sh
   gh workflow run publish-staged-prerelease.yml --ref main \
     -f tag=v0.21.0-rc1 -f target_sha=<40-hex> \
     -f app_notarize_run_id=<id> -f cli_notarize_run_id=<id> \
     -f app_asset_sha256=<hex> -f cli_binary_sha256=<hex> \
     -f provenance_note='...'
   ```

3. **If `publish` fails after creating the draft**, inspect the draft. Delete
   it deliberately before dispatching again; the workflow refuses while any
   release exists for the tag.

## Permissions and limitations

- **Permissions.** `contents: write` (to create and publish the release) and
  `actions: read` (to read notarize runs and download their outputs). The
  token could technically push to branches, but the script only performs the
  calls described above. Review the script, not just the permissions block.
- **Tag type.** The tag is lightweight. It is created by publishing the
  release, so no annotated message is attached.
- **Runner.** The workflow runs on `ubuntu-latest`. It does not re-verify code
  signatures; that was done on macOS by the notarize runs, whose
  Apple-accepted log binding is re-checked here.

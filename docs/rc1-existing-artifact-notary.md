# RC1 existing-artifact notary runbook (PARTIAL: auth-only)

Operational runbook for the `rc1-existing-artifact-notary.yml` workflow.

**This is a partial implementation.** The current workflow is
**auth-only**: it verifies the App Store Connect notary credential path with a
read-only `xcrun notarytool history` probe and nothing else. There is
deliberately **no** submission, stapling, DMG/tarball download, artifact
verification, release/tag write, or product build in this workflow yet. Future
artifact verification and submission phases must be separately designed,
reviewed, and merged before any notarization is attempted.

## Status

| Phase | Status |
| --- | --- |
| Trusted-dispatch guard (`workflow_dispatch`, pinned repo, default branch) | Implemented |
| Auth-only notary preflight (`notarytool history`, no submission) | Implemented |
| Artifact download + hash/metadata verification of the fixed RC1 artifacts | **Not implemented** — future, separately reviewed |
| One-shot notary submission of the desktop DMG | **Not implemented** — future, separately reviewed |
| Stapling/validation/retention of the notarized DMG | **Not implemented** — future, separately reviewed |

## What it authenticates against (pinned identities, not processed)

The pinned identities below describe the RC1 candidate the future phases will
target. The auth-only workflow does not download, verify, or touch any of
these artifacts; the originals are immutable.

| Item | Value |
| --- | --- |
| Repository | `gents-ai/gents` |
| Source SHA | `08edd1bdfedca73fa28e648e4781ae03aeacf5e6` |
| Build run | `39302246819` |
| Desktop artifact | `12217555726` (`gents-desktop_0.21.0-rc1_aarch64-signed-unnotarized-candidate.dmg`, sha256 `c6ee15a715e0a66a0d55ec435ddff127a52319bf10ad40ad1d6da134403245d7`) |
| CLI artifact | `12214205657` (`gents-aarch64-apple-darwin-signed-unnotarized-candidate.tar.gz`, sha256 `71979526db36097b7b145398e7f7a7cfc47fb9d33ab5ea0a23eed7fe398caf55`, inner binary `5b488fc5af64f0a0112ab97b0894b2b7a2e4728df5b8c481a3f7c4b7aa3a2295`) |
| Desktop manifest | `candidate-build.json`, sha256 `b53772b976b4572f5a79d177748dbad3eaee3b60e26e1d23e892bbd647aadcb2` |
| Inner binaries | app `076ad259e4451eeb4a46b0af336278b472aeb6fea392f02f39231618d19dd2d3`, CLI `64f5f84e22ae0056fead492dcc42d695d928b1f272ee744ea99e9dd5c5454abf`, bridge `ff17f13c68272e5b833b0a8a42f3d46fbabb1eee6964fe85e388205a7f3b3deb` (to be bound via the hash-verified manifest) |
| Codesign identity (expected) | Team `NXJ753WW5X`, Developer ID Application; cert SHA-1 `4855F05BB34BFB227F828F9856DA05D2D02E7D0F`, SHA-256 `FFE161226E3BA89575FBB2FA4E0FFC2F7B0A7C99DE8FC2F696A24FB8D74F1BAF` (the embedded signature would remain authoritative; no keychain import, no resign) |

## Prerequisites

- The workflow file is registered on the repository **default branch**
  (`main`). Registration is an Engineer-merged change — the automation that
  authored the workflow never merges, pushes, or dispatches. Until it lands on
  `main`, the workflow cannot be dispatched at all.
- Runner labels `[self-hosted, macOS, ARM64, studio, studio-1, ci-support]`
  resolve to a healthy machine with Xcode command-line tools.
- Repository secrets `MACOS_NOTARY_API_KEY`, `MACOS_NOTARY_ISSUER_ID`,
  `MACOS_NOTARY_KEY_ID` (App Store Connect API key with App Manager or
  Developer role on the notary team).

## Running it

1. **Dispatch** the workflow from the Actions tab on `main`. There are no
   dispatch inputs; every run is auth-only.
2. The run performs exactly one external action: a single read-only
   `xcrun notarytool history` probe. Nothing is uploaded to Apple and no
   notary state changes.

## Phases

1. **`repository-guard`** — asserts `workflow_dispatch` from `gents-ai/gents`
   on the branch named by the explicit `github.event.repository.default_branch`
   expression (empty expression fails closed), then shellchecks the auth
   helper, syntax-checks it, and runs both mocked test suites
   (`test-rc1-notary-verify.py`, `test-rc1-notary-workflow.py`).
2. **`notary-auth-preflight`** — invokes `.github/scripts/rc1-notary-auth.sh`
   once (no retry loop). The helper materializes the `.p8` in a `0700` temp
   dir (key file `0600`), captures the full `notarytool history` output and
   errors in private `0600` files inside that dir, emits only the allowlisted
   success line or a fixed HTTP 403 / auth-failure diagnostic, and removes the
   key material and captured output via EXIT/INT/TERM traps on every path.

## Secrets handling

- Secrets are read from the repository secret store only through the step
  `env:` mapping in the workflow; no shell reads or prints them.
- The `.p8` exists only inside the `0700` temp dir as a `0600` file and is
  deleted by the cleanup trap on success, failure, signal, and script exit.
- Raw notary history/error output is never echoed; only the allowlisted
  diagnostic lines above may appear in job logs.

## What this workflow never does

- No notary submission, no stapling, no release or tag write, no product
  build, no artifact download, no re-signing.
- No execution of any candidate CLI/app/bridge binary or staged script.
- No writes to releases, tags, or repository contents (permissions are
  `contents: read`, `actions: read` only).
- No source changes to the product at `08edd1bdfedca73fa28e648e4781ae03aeacf5e6`;
  the desktop (`12217555726`), CLI (`12214205657`), and build-run
  (`39302246819`) originals remain immutable.

## Future phases (not in this workflow)

Artifact verification, one-shot submission, and stapling of the pinned DMG
require their own design and review before implementation; they must never be
re-added implicitly. When implemented, they should keep the fail-closed
properties documented previously: durable receipts written before any
branching, rerun-refusal, work-copy-only stapling with immutable originals,
and fully sanitized diagnostics.

## Local verification

```sh
python3 .github/scripts/test-rc1-notary-workflow.py   # workflow structure tests
python3 .github/scripts/test-rc1-notary-verify.py     # mocked verifier/auth tests
sh -n .github/scripts/rc1-notary-auth.sh
shellcheck -s sh .github/scripts/rc1-notary-auth.sh
```

The auth helper is the only notary-capable script; it never submits.

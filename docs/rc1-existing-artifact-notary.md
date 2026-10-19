# RC1 existing-artifact notarization runbook

Operational runbook for the `rc1-existing-artifact-notary.yml` workflow: a
CI-control-only notarization of the already built, already signed RC1 desktop
candidate. It performs **no** product build, no re-signing, no release or tag
write, and never executes the candidate binaries.

## What it processes (pinned identities)

Everything is pinned; a mismatch anywhere fails the run closed.

| Item | Value |
| --- | --- |
| Repository | `gents-ai/gents` |
| Source SHA | `08edd1bdfedca73fa28e648e4781ae03aeacf5e6` |
| Build run | `39302246819` |
| CLI artifact | `12217555726` (`gents-aarch64-apple-darwin-signed-unnotarized-candidate.tar.gz`, sha256 `71979526db36097b7b145398e7f7a7cfc47fb9d33ab5ea0a23eed7fe398caf55`, inner binary `5b488fc5af64f0a0112ab97b0894b2b7a2e4728df5b8c481a3f7c4b7aa3a2295`) |
| Desktop artifact | `12214205657` (`gents-desktop_0.21.0-rc1_aarch64-signed-unnotarized-candidate.dmg`, sha256 `c6ee15a715e0a66a0d55ec435ddff127a52319bf10ad40ad1d6da134403245d7`) |
| Desktop manifest | `candidate-build.json`, sha256 `b53772b976b4572f5a79d177748dbad3eaee3b60e26e1d23e892bbd647aadcb2` |
| Inner binaries | app `076ad259e4451eeb4a46b0af336278b472aeb6fea392f02f39231618d19dd2d3`, CLI `64f5f84e22ae0056fead492dcc42d695d928b1f272ee744ea99e9dd5c5454abf`, bridge `ff17f13c68272e5b833b0a8a42f3d46fbabb1eee6964fe85e388205a7f3b3deb` (bound via the hash-verified manifest) |
| Codesign identity | Team `NXJ753WW5X`, Developer ID Application; cert SHA-1 `4855F05BB34BFB227F828F9856DA05D2D02E7D0F`, SHA-256 `FFE161226E3BA89575FBB2FA4E0FFC2F7B0A7C99DE8FC2F696A24FB8D74F1BAF` (embedded signature is authoritative; no keychain import, no resign) |

## Prerequisites

- The workflow file is registered on the repository **default branch**
  (`main`). Registration is an Engineer-merged change — the automation that
  authored the workflow never merges. Until it lands on `main`, the workflow
  cannot be dispatched at all.
- Runner labels `[self-hosted, macOS, ARM64, studio, studio-1, ci-support]`
  (guard/preflight) and `[self-hosted, macOS, ARM64, studio, studio-2,
  ci-desktop]` (submit/staple) resolve to healthy machines with Xcode
  command-line tools.
- Repository secrets `MACOS_NOTARY_API_KEY`, `MACOS_NOTARY_ISSUER_ID`,
  `MACOS_NOTARY_KEY_ID` (App Store Connect API key with App Manager or
  Developer role on the notary team).

## Running it

1. **Dispatch** the workflow from the Actions tab on `main`.
2. Leave **`submit` = `false`** for an auth-only dry run: only the repository
   guard and the notary credential preflight (`notarytool history`) execute.
   Nothing is uploaded to Apple.
3. Set **`submit` = `true`** to admit the one-shot notary submission of the
   pinned DMG. Submission is explicitly admitted; it is never implicit.

## Phases

1. **`repository-guard`** — asserts `workflow_dispatch` on the default branch
   of `gents-ai/gents`, then shellchecks the control scripts and runs the
   mocked test suite (`python3 .github/scripts/test-rc1-notary-verify.py`).
2. **`notary-auth-preflight`** — materializes the `.p8` into a `0700` temp
   dir, runs read-only `xcrun notarytool history --output-format json`, then
   removes the key material unconditionally. HTTP 403 produces a sanitized
   error and a hard stop; there are no retry loops.
3. **`notary-submit`** (`submit=true` only) — downloads the two pinned
   artifacts, re-verifies run `head_sha` and every pinned hash, checks that no
   durable submission receipt exists yet, then submits the DMG once with
   `xcrun notarytool submit --wait`. The receipt is written durably **before**
   any further branching and uploaded as a workflow artifact. If a receipt
   already exists, the job fails closed instead of resubmitting.
4. **`staple-validate`** — re-downloads and re-verifies everything, copies the
   DMG to a mutable work copy (the original is made read-only), verifies the
   existing Developer ID signature with `codesign --verify --strict --deep`,
   staples the notarization ticket with `xcrun stapler staple`, validates with
   `xcrun stapler validate`, regenerates `SHA256SUMS-desktop-macos.txt`, and
   uploads the stapled DMG + checksums as workflow artifacts.

## Receipts, reconciliation, and reruns

- The receipt artifact `rc1-notary-submission-receipts-<run_id>` records the
  notary submission id, run id, attempt, source SHA, DMG sha256, and captured
  output. Keep it: it is the durable proof of what was submitted.
- **Rerun of a run that already submitted fails closed.** To recover after a
  partial run, use the receipt's submission id to reconcile against
  `xcrun notarytool history` (read-only); only notarization *accepted* state
  from that receipt may justify re-stapling — never a second submission of
  the same bytes.
- Receipts and logs are non-secret and safe to retain; the `.p8` never is.

## What this workflow never does

- No builds, installs, or dependency changes; no node configuration changes.
- No execution of the candidate CLI/app/bridge binaries or any staged script.
- No writes to releases, tags, or repository contents (permissions are
  `contents: read`, `actions: read` only).
- No re-signing: the existing Developer ID signature is verified, not replaced.

## Local verification

```sh
python3 .github/scripts/test-rc1-notary-verify.py   # mocked tests, no Apple calls
shellcheck -s sh .github/scripts/rc1-notary-auth.sh
python3 .github/scripts/rc1-notary-verify.py --help
```

The verifier is hash/metadata-only; it never mounts the DMG or extracts the
tarball to disk.

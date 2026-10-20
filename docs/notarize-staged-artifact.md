# Notarize a staged, already-signed macOS artifact

Workflow: `.github/workflows/notarize-staged-artifact.yml`
Script: `.github/scripts/notarize-staged-artifact.sh verify|submit|complete`
(+ `notary_json.py`)
Tests: `python3 .github/scripts/test-notarize-staged-artifact.py` (macOS; the
static workflow checks need PyYAML and are skipped without it). These tests
are **not** wired into `ci.yml`; run them manually before changing these files.

Use this workflow when a Developer-ID-signed `.app` already exists and only
notarization and stapling are missing. It does not build, re-sign or publish
anything. Its only GitHub permission is `contents: read`. Hash equality plus
the pinned signer establishes *which bytes and which signer*. It does not
establish that the artifact is an approved release candidate; selecting the
candidate and publishing it are separate operator decisions.

## Phases

The workflow runs as one job with these steps, in order:

1. **Trusted-context guard**, checkout, tool preflight.
2. **`verify` (no credentials in the step environment).**
   - Copies `<inbox>/<sha256>.zip` privately and checks its SHA-256.
   - Validates every archive entry **before** extraction:
     - No absolute paths, empty/`.`/`..` components, backslashes, NUL or
       duplicate names.
     - Only regular files, directories and symlinks.
     - Exactly one top-level `<Name>.app`, which is not itself a symlink.
     - No entry beneath a symlink entry.
     - Symlink targets are relative and stay inside the app.
     - `__MACOSX/` is rejected.
   - Extracts the archive, then re-checks the real tree: only the app at top
     level, the app is a real directory, and every symlink's real path stays
     inside the app.
   - Checks the signature: strict deep `codesign`, TeamIdentifier
     `NXJ753WW5X`, bundle id `com.source-inc.gents`, the hardened runtime, and
     the pinned leaf certificate SHA-256.
   - In submit mode, the app must not already be stapled.
   - Candidate binaries are never executed. The step refuses to run if notary
     secrets are in its environment.
3. **`submit` (credentials).**
   - **Submit mode:**
     - If `NOTARY_STATE_DIR/<full sha256>/` exists, the step refuses before
       any Apple call.
     - Otherwise it checks Apple history strictly. It fails closed if the
       `history` list is missing, if any entry lacks a string
       `id`/`name`/`status`, or if any entry already has this artifact's
       name.
     - It then atomically creates `intent.json` in that host state directory
       and runs exactly one `notarytool submit --no-wait`.
     - It announces the id (notice, summary, step output) and writes
       `receipt.json`.
     - If submit exits non-zero or returns no id, it writes
       `outcome.json` with `state: uncertain` and fails. Any later submit for
       that artifact is refused until the uncertainty is reconciled.
   - **Resume mode:**
     - Never submits and never reads history.
     - The id must match any recorded `receipt.json`.
     - Apple's `info` for the id must report this artifact's submission name.
       This is checked **before** any waiting.
4. **Upload submission receipt.** Uploads `out/` (intent, receipt, outcome)
   as an artifact **before** the long wait, even if submit failed.
5. **`complete` (credentials).**
   - Runs one `notarytool wait`, then `info`, whose id and name must match.
   - `In Progress` means: resume later.
   - Any other status that isn't `Accepted` fails; the log is kept if
     retrievable.
   - For `Accepted`, Apple's log **must** be retrievable. Its `jobId` must
     equal the submission id, its `sha256` must equal the **full** staged
     SHA-256, and its status must be `Accepted`. Otherwise the step fails
     without stapling.
   - It then staples and checks `stapler validate`, strict `codesign`, and
     `spctl` reporting `source=Notarized Developer ID`. Finally it re-zips the
     app, writes `SHA256SUMS`, and records `accepted-<id>.json` in host state.
6. **Upload outputs.** Always. Contains the receipt, `notary-info*.json`,
   `notary-log.json`, the stapled zip and `SHA256SUMS`.
7. **Remove the per-run work directory.** Always. The directory is
   `${RUNNER_TEMP}/notarize-<run_id>-<attempt>`, unique per attempt; verify
   refuses to reuse one.

## Data handling

- **Credentials** live only in the submit and complete steps. Each step
  writes them to its own 0700 directory under `RUNNER_TEMP` as `AuthKey.p8`
  (mode 0600). The directory is removed on EXIT/INT/TERM. That cleanup cannot
  cover SIGKILL or runner loss, so secrecy ultimately rests on the API key
  being revocable.
- **Raw notarytool stderr** is captured privately and never printed. Errors
  are reported as fixed messages; a 403 gets a neutral one.
- **Printed provider data.** Some provider-derived values *are* printed:
  submission ids, names, statuses and dates. Each is printed only if it
  matches `[A-Za-z0-9._:+ -]{1,128}`; otherwise `<redacted>` is printed.
- **Uploaded provider data.** `notary-info*.json` and `notary-log.json` are
  Apple provider data (non-secret) and are uploaded as artifacts as-is.

## Duplicate-guard limitations (read before dispatching)

- **The host state directory is the primary authority.** It is
  `/Users/admin/gents-notary-state` on studio-1, where this workflow always
  runs. It is host-local:
  - Submissions made from any other machine or workflow are not recorded
    there.
  - Deleting or editing it removes the guard.
- **Apple history is a secondary check only.** `notarytool history`
  (notarytool 1.1.x) has no pagination options and documents no
  completeness guarantee. An empty or non-matching history is therefore never
  treated as proof that no submission exists.
- **Other naming schemes are not detected.** Submissions made under other
  names, such as `release-macos.yml` uploads of the same app, do not match
  this workflow's name `gents-notary-<sha16>.zip`.
- **A rejected submission blocks resubmission.** If a submission for an
  artifact ended `Invalid`, its state blocks future submits for that
  artifact. Fix the cause and stage a **different** artifact (new SHA). Only
  remove the state after deliberate reconciliation.

## Operator procedure

1. **Stage the zip** on studio-1. It must be readable by the runner user
   `admin`.

   ```sh
   SHA=$(shasum -a 256 Gents.zip | cut -d' ' -f1)
   ssh studio-1 'mkdir -p /Users/admin/gents-notary-inbox'
   scp Gents.zip "studio-1:/Users/admin/gents-notary-inbox/${SHA}.zip"
   ssh studio-1 "chmod 0444 /Users/admin/gents-notary-inbox/${SHA}.zip && shasum -a 256 /Users/admin/gents-notary-inbox/${SHA}.zip"
   ```

2. **Dispatch once** from the default branch:

   ```sh
   gh workflow run notarize-staged-artifact.yml --ref main \
     -f artifact_sha256="$SHA" -f mode=submit
   ```

3. **If anything is ambiguous after a submission id was announced**, resume.
   Ambiguous means a timeout, In Progress, runner loss, or any failure in
   `complete`. Do **not** dispatch submit again:

   ```sh
   gh workflow run notarize-staged-artifact.yml --ref main \
     -f artifact_sha256="$SHA" -f mode=resume -f submission_id=<id>
   ```

4. **If submit was recorded as uncertain** (no id announced):
   - Inspect `/Users/admin/gents-notary-state/<sha256>/` and Apple's
     submission history out of band.
   - If a submission exists, resume it by id.
   - Only if you have established that no submission was created, move the
     state directory aside (keep it as evidence) and dispatch submit again.
5. **Download the outputs:** the `notarize-staged-<run>-<attempt>` artifact.
   Publishing is a separate, explicit step.

## Other limitations

- A raw `.zip` cannot be stapled. The stapled `.app` is re-zipped, so the
  output zip's SHA-256 differs from the input's.
- No DMG handling.
- No bare-binary (CLI) notarization.
- No publishing.

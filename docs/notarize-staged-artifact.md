# Notarize a staged, already-signed macOS artifact

Workflow: `.github/workflows/notarize-staged-artifact.yml`  
Script: `.github/scripts/notarize-staged-artifact.sh` (+ `notary_json.py`)  
Tests: `python3 .github/scripts/test-notarize-staged-artifact.py` (macOS; the
static workflow checks need PyYAML and are skipped without it).

Use this when a Developer-ID-signed `.app` already exists, for example a
manually built release candidate, and only notarization and stapling are
missing. Nothing is built or re-signed, and nothing is published. The
workflow's only GitHub permission is `contents: read`.

## What it guarantees

- **Verification comes before credentials.** Checks run in this order: the
  artifact SHA-256, exactly one top-level `.app`, strict deep `codesign`
  verification, TeamIdentifier `NXJ753WW5X`, bundle identifier
  `com.source-inc.gents`, the hardened-runtime flag, and the pinned leaf
  certificate SHA-256 (`ffe16122…1baf`). In submit mode the app must not
  already be stapled. Notary credentials are written to disk only after all
  of these pass. Candidate binaries are never executed.
- **One submission per artifact.** The submission name is derived from the
  artifact: `gents-notary-<first 16 hex of sha256>.zip`. Before submitting,
  the workflow reads Apple's `notarytool history`. If any entry already has
  that name, it fails without submitting. It also fails without submitting if
  history is unavailable or cannot be parsed. Because Apple's history is the
  record, this guard also covers reruns and new dispatches.
- **The receipt is written before waiting.** There is exactly one
  `notarytool submit --no-wait` call. `receipt.json` (submission id, name,
  artifact SHA, run) is written, the id is announced as a workflow notice and
  in the step summary, and only then does the workflow wait. The output
  directory is uploaded with `if: always()`.
- **Ambiguous outcomes are resumed, never resubmitted.** Use `mode=resume`
  with the submission id. Resume never calls `submit`. It refuses ids whose
  submission name does not match this artifact.
- **Acceptance gates.** The submission status must be `Accepted`; its
  notarization log is retained. After that, the workflow staples the
  extracted `.app` and checks `stapler validate`, strict `codesign` again, and
  `spctl` reporting `source=Notarized Developer ID`. It then re-zips the app
  with `ditto -c -k --keepParent` and writes `SHA256SUMS`.
- **Credential handling.** This mirrors `rc1-notary-auth.sh`: a 0700 dir under
  `RUNNER_TEMP`, a constant `AuthKey.p8` (0600), raw notarytool stderr
  captured privately, and only fixed diagnostics printed (a 403 gets a
  neutral message). Cleanup runs on EXIT/INT/TERM. It cannot cover SIGKILL or
  runner loss, so secrecy ultimately rests on the API key being revocable.

## Operator procedure

1. Compute the zip's SHA-256 and stage it on the studio-1 host. The inbox
   path is fixed:

   ```sh
   SHA=$(shasum -a 256 Gents.zip | cut -d' ' -f1)
   ssh studio-1 'mkdir -p /Users/admin/gents-notary-inbox'
   scp Gents.zip "studio-1:/Users/admin/gents-notary-inbox/${SHA}.zip"
   ssh studio-1 "chmod 0444 /Users/admin/gents-notary-inbox/${SHA}.zip && shasum -a 256 /Users/admin/gents-notary-inbox/${SHA}.zip"
   ```

2. Dispatch once from the default branch:

   ```sh
   gh workflow run notarize-staged-artifact.yml --ref main \
     -f artifact_sha256="$SHA" -f mode=submit
   ```

3. If the run fails or times out **after** it has announced a submission id,
   do not dispatch `submit` again. Re-dispatch with:

   ```sh
   gh workflow run notarize-staged-artifact.yml --ref main \
     -f artifact_sha256="$SHA" -f mode=resume -f submission_id=<id>
   ```

   If a submit-mode run is re-dispatched by mistake, the history guard stops
   it and prints the existing id to resume.

4. Download the `notarize-staged-<run>-<attempt>` artifact. It contains
   `receipt.json`, `notary-info.json`, `notary-log.json`, the stapled
   `<App>-<sha16>-notarized.zip` and `SHA256SUMS`. Publishing is a separate,
   explicit step.

## Limitations

- The duplicate guard depends on `notarytool history` listing earlier
  submissions for the same API-key team. A submission made under a different
  team or credential cannot be seen.
- A raw `.zip` cannot be stapled; the stapled `.app` is re-zipped, so the
  output zip's SHA-256 differs from the input's.
- The workflow does not create or sign a DMG.
- The workflow does not publish releases.

# Notarize a staged, already-signed macOS artifact

Workflow: `.github/workflows/notarize-staged-artifact.yml`
Script: `.github/scripts/notarize-staged-artifact.sh verify|submit|complete`
(+ `notary_json.py`)
Tests: `python3 .github/scripts/test-notarize-staged-artifact.py`. They need
macOS (real `ditto`/`shasum`); the static workflow checks need PyYAML. The
`notary-control-tests` job in `ci.yml` runs them, with PyYAML, on every pull
request.

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
     - No absolute paths, empty/`.`/`..` components, backslashes or NUL.
     - **Names are compared as the runner's filesystem compares them.** The
       studio-1 data volume is APFS, which is case-insensitive and
       Unicode-normalization-insensitive; the tests confirm both properties
       on the test host. Names are therefore compared by an alias key (NFD,
       then casefold). Two entries with the same key are rejected, and so is
       any entry beneath an alias of a symlink entry. This includes
       `Link`/`link/x`, and `café` (NFC) versus `café` (NFD).
     - Only regular files, directories and symlinks.
     - Exactly one top-level `<Name>.app`, which is not itself a symlink.
     - **Conservative symlink policy.** A target must be relative and must not
       contain empty, `.` or `..` components. Such a target can only descend
       from the link's own directory, which is inside the app. So no chain of
       links can leave the app, and no link resolution is needed. Legitimate
       links that climb with `..` are rejected rather than resolved; framework
       links (`Versions/Current -> A`, `X -> Versions/Current/X`) pass. The
       `039e22998` desktop zip contains no symlinks.
     - `__MACOSX/` is rejected. See the operator procedure for how to build
       the zip.
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
     - It then **durably** creates the state directory and `intent.json`
       (see *Host-state durability*). Only after that succeeds does it run
       exactly one `notarytool submit --no-wait`. If any part of the
       persistence fails, it refuses to submit (exit 15).
     - It announces the id (notice, summary, step output), writes
       `out/receipt.json`, then durably writes the host `receipt.json`. If
       the host receipt cannot be persisted, it fails (exit 16) telling the
       operator to resume with that id. `intent.json` still blocks a resubmit.
     - If submit exits non-zero or returns no id, it records
       `outcome.json` with `state: uncertain` (best effort) and fails. Any
       later submit for that artifact is refused, because the intent exists,
       until the uncertainty is reconciled.
   - **Resume mode:**
     - Never submits and never reads history.
     - Durably creates the host state directory for the artifact if it does
       not exist yet. After Apple's identity check, it durably records
       `receipt.json` (`state: resumed`) if none exists. A resumed submission
       therefore blocks later submit-mode runs, even when the original
       submission was made elsewhere.
     - Submission ids are lowercased, so uppercase input works.
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

## Bare executables (`artifact_kind=cli`)

Use `artifact_kind=cli` for a Developer-ID-signed command-line executable.
The default is `app`. The app path is unchanged by this option.

- **Staging.** Zip the single binary with `ditto -c -k gents gents.zip`; the
  archive must contain exactly one entry, named `gents`.
- **Archive checks.** The single entry must be a regular file named `gents`.
  Directories, symlinks, extra entries and nested paths are rejected.
- **Signature checks.** Strict `codesign`, identifier `gents`,
  TeamIdentifier `NXJ753WW5X`, the hardened runtime, the pinned leaf
  certificate, and `Format=Mach-O`. The verify step records the executable's
  SHA-256 and its CDHash.
- **Stapling.** Bare executables cannot carry a stapled ticket. Instead,
  `complete` requires everything the app path requires (Accepted, Apple's log
  binding the job id and the full archive SHA-256), and in addition requires
  the log's `ticketContents` to contain the verified CDHash. It fails closed
  if `ticketContents` is missing or does not match. It then re-verifies the
  signature and the executable's SHA-256 and copies the executable
  **byte-for-byte unchanged** to the output, with its own `SHA256SUMS`.
- **Gatekeeper.** Gatekeeper fetches the ticket online the first time a
  quarantined copy runs. Offline first launches of quarantined copies cannot
  be validated.
- **Pinning the executable.** The staged zip's SHA-256 depends on the zip's
  timestamps. The executable's SHA-256 is the stable identity, so compare the
  printed executable SHA-256 and the output `SHA256SUMS` against the build
  receipt before publishing.

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

## Host-state durability

Records are created by `notary_json.py` as follows:

- **State root.** Created if missing (mode 0700). Its parent directory is
  flushed, then the root itself is flushed.
- **Artifact directory.** `<root>/<full sha256>/` is created with an
  exclusive `mkdir`; an existing directory refuses the submit. The root is
  then flushed so the new directory entry is stable.
- **Each record** (`intent.json`, `receipt.json`, `outcome.json`,
  `accepted-<id>.json`) is published in four steps:
  1. The JSON is written to a temporary file.
  2. The file is flushed with `fsync` plus `F_FULLFSYNC`.
  3. The file is hard-linked to its final name. The link fails if the name
     exists, so records are never overwritten and are never visible
     half-written.
  4. The temporary name is removed and the directory is flushed.
- **Errors.** Every flush and link error is a failure, and the script fails
  closed.

`F_FULLFSYNC` is what asks macOS to push data through the drive's write cache
to stable storage; plain `fsync` does not guarantee that. It is applied to
files and directories, and works on directories on studio-1 (checked).

What this guarantees, and what it does not:

- **Process interruption** (cancel, timeout, SIGKILL, runner restart) with the
  host still up: every record that was written is visible.
  - An interruption before the intent exists means no Apple contact happened.
  - An interruption after it means the intent blocks any automatic resubmit.
- **Host crash or power loss.** The intent, including its directory entries,
  has reached stable storage before `notarytool submit` starts. A crash
  between the submit and a durable receipt leaves an intent with no receipt.
  That is reconciled by hand (operator step 4), never resubmitted
  automatically.
- **Not covered:**
  - Hardware that ignores cache-flush requests.
  - Loss or corruption of the volume.
  - Deleting the state directory or replacing the host.
  - Submissions made from anywhere other than this host.

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

   Build the zip with `ditto -c -k --keepParent Gents.app Gents.zip`.
   **Do not** pass `--sequesterRsrc`: it adds a top-level `__MACOSX/` folder,
   which the single-top-level-entry check rejects.

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
- No publishing.

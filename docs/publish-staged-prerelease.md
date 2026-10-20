# Publish a staged macOS prerelease

Workflow: `.github/workflows/publish-staged-prerelease.yml`
Script: `.github/scripts/publish_staged_prerelease.py collect|publish`
Tests: `python3 .github/scripts/test-publish-staged-prerelease.py`. All GitHub
calls go through a fake `gh`. The static workflow checks need PyYAML. The
`publish-control-tests` job in `ci.yml` runs them, with PyYAML, on pull
requests.

This workflow publishes a **new** macOS prerelease (`vX.Y.Z-rcN`) from the
outputs of two successful `notarize-staged-artifact.yml` runs: one for the
desktop app and one for the CLI. It never builds, never signs, and never
modifies an existing tag or release.

## Candidate approval and source provenance

The target SHA is chosen by the operator. The workflow only checks that the
commit exists. Nothing in it proves that the notarized binaries were built
from that commit. Two inputs are therefore required:

- **`candidate_approval_url`**: a link to the record in this repository that
  approves the exact candidate, for example the owner's confirmation comment.
  Dispatch only after that approval exists. This is a mandatory operator gate;
  the workflow can check the link's form, not its content.
- **`provenance_note`**: the basis for linking the binaries to the commit.
  Examples are embedded build stamps or build receipts. State any gaps, such
  as a build manifest whose `source_sha` is null.

**Branch reachability.** The target must be reachable from the default
branch, checked with `compare <default>...<target>` returning `identical` or
`behind`. If it is not, the workflow refuses and never substitutes another
commit. Publishing a commit that is only on another branch requires the
explicit `target_branch` input. That branch must contain the target, and the
notes then say the commit is **not** on the default branch and name the
branch and its head. `publish` re-checks this, and refuses if the result
changed since `collect` (for example, the branch moved). The tag keeps the
commit reachable after publication, even if the branch is later deleted.

The release notes show the SHA as **"Tag target (operator-selected)"**. They
quote the provenance note and the approval link, and state that neither a
clean source tree nor a reproducible build is established.

## Why the release rebuild workflows should not start

Pushing a `v*` tag normally starts `release-macos`,
`release-desktop-packages`, `release-linux` and `release-linux-desktop`.
Those workflows rebuild and upload assets, and `release-macos` uploads with
`--clobber`.

This workflow creates the tag ref, the draft release and the publication
with its own explicitly supplied `GITHUB_TOKEN`. **The preventive control is
GitHub's documented behavior:** events created by `GITHUB_TOKEN` do not start
new workflow runs, except `workflow_dispatch` and `repository_dispatch`. No
repository settings and no other credentials are involved. The tests use a
fake `gh`, so they cannot demonstrate this service behavior.

After publishing, the script performs a **bounded observation**. After a
short delay, it lists workflow runs whose head branch is the tag, for any
event. A malformed listing is an error. Any listed run fails the step, so it
can be inspected and cancelled. An empty listing at that moment is evidence,
not proof that no run will start.

## Jobs and permissions

The workflow has no workflow-level permissions. It runs two jobs:

- **`collect`** has `contents: read` and `actions: read`. It verifies
  everything and builds the assets, notes and manifest, then uploads them as
  an artifact of this run.
- **`publish`** has `contents: write` and nothing else, and runs only after
  `collect` succeeds. It downloads that artifact and re-verifies it against
  its own inputs before writing anything.

A read-only token cannot see draft releases. `collect`'s absence check
therefore covers tags and published releases. `publish` repeats the check
with the write token, which can see drafts.

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
  - Its `head_sha` is reachable from the default branch.
  - **Same reviewed implementation.** At the run's `head_sha`, the
    notarization workflow, `notarize-staged-artifact.sh` and `notary_json.py`
    must have the same git blob as at this publishing workflow's own commit
    (`github.sha`). So a run produced by a different, unreviewed version of
    the notarization code is refused. If those files change on `main`
    between notarizing and publishing, notarize again with the new version
    first.
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

1. Re-verifies the downloaded manifest against this job's inputs:
   - The tag, target and pinned hashes match.
   - The asset set is exactly the expected four files.
   - Every asset hash matches.
   - The app asset equals `app_asset_sha256`.
   - The CLI tarball contains `cli_binary_sha256`.

   It then re-checks the target commit and its reachability (which must be
   unchanged since `collect`), and that the tag and any release, drafts
   included, are still absent.
2. **Creates the tag exclusively.** It calls `POST /git/refs` for
   `refs/tags/<tag>` at the target SHA. GitHub rejects this if the ref
   already exists (HTTP 422). The step then fails, and the existing ref is
   **never** updated or forced. It then reads the ref back and requires a
   commit ref at the target.
3. Runs `gh release create <tag> --verify-tag --draft --prerelease
   --latest=false` with the assets. `--verify-tag` requires the tag created
   in step 2, so `gh` cannot create a different tag.
4. Requires exactly one draft for the tag, flagged as a prerelease. Its asset
   names must equal the manifest exactly, and each GitHub-computed `digest`
   must equal the local SHA-256. On any mismatch the release is **left as a
   draft** and the step fails.
5. **Immediately before publishing**, reads the tag ref again and refuses
   (leaving the draft) unless it is still a commit ref at the target.
6. Publishes by PATCHing `draft: false`, then reads the tag ref once more.
7. Waits briefly and runs the bounded observation described above.

## Recovery

The workflow never deletes or moves anything. It refuses whenever the tag or
a release for it already exists. Recovery is a deliberate operator action:

- **Failure before step 2.** Nothing was written; dispatch again.
- **Conflict in step 2** (the tag already existed). Find out who created it
  and where it points. Under exclusive publication ownership this should not
  happen. Do not delete another publisher's tag. Resolve ownership first.
- **Failure after step 2** (draft create failed, digest mismatch, tag moved).
  1. The tag exists, possibly with a draft.
  2. Inspect both. Confirm the tag points at the intended commit and that no
     workflow runs started for it.
  3. Delete the draft, then the tag, using an authorized account.
  4. Record what was deleted and why in the approval thread.
  5. Dispatch again.
- **Failure after step 6** (the release is published). Do not unpublish
  automatically. Inspect the release and any observed runs, and cancel runs
  that could modify assets.

## Operator procedure

1. Confirm a candidate-approval record exists for the exact artifacts. Check
   whether the target is on the default branch; if it is not, decide
   explicitly whether to publish it from its branch (`target_branch`). Also
   confirm that the artifacts' architecture matches what the notes claim
   (`lipo -archs` on every executable; the notes say arm64 only).
2. Finish both `notarize-staged-artifact.yml` runs (app and CLI). Note their
   run ids, the app zip SHA-256 from the app run's `SHA256SUMS`, and the CLI
   binary SHA-256. Check the CLI SHA-256 against the build receipt.
3. Dispatch from the default branch:

   ```sh
   gh workflow run publish-staged-prerelease.yml --ref main \
     -f tag=v0.21.0-rc1 -f target_sha=<40-hex> \
     -f app_notarize_run_id=<id> -f cli_notarize_run_id=<id> \
     -f app_asset_sha256=<hex> -f cli_binary_sha256=<hex> \
     -f provenance_note='...' \
     -f candidate_approval_url='https://github.com/gents-ai/gents/pull/<n>#issuecomment-<id>' \
     -f target_branch=<only if target_sha is not on main>
   ```

4. **If `publish` fails**, follow *Recovery* above. The workflow refuses
   while the tag or any release for it exists.

## Permissions and limitations

- **Permissions.** `contents: write` (to create the tag ref and the release)
  and `actions: read` (to read notarize runs and download their outputs). The
  token could technically push to branches, but the script only performs the
  calls described above. Review the script, not just the permissions block.
- **Tag type.** The tag is a lightweight tag created through the refs API, so
  no annotated message is attached.
- **Not covered by the exclusive create:** concurrent publishers. The ref
  create is exclusive, and the ref is re-verified before publishing, but
  another party with write access could still act on the tag afterwards.
  Publication ownership must be exclusive; it was assigned for
  `v0.21.0-rc1`.
- **Runner.** The workflow runs on `ubuntu-latest`. It does not re-verify code
  signatures; that was done on macOS by the notarize runs, whose
  Apple-accepted log binding is re-checked here.

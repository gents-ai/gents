# Defending-code pack

This pack adapts the static find-and-fix workflow from Anthropic's
`defending-code-reference-harness` into a Gents-native document graph. It is
deliberately different from `security-scan`: there is no regex kickoff and no
generic datastore console. The graph starts by building a threat model, uses
that model to partition a static review, adversarially verifies and ranks the
candidate ledger, drafts patches as documents, independently reviews those
diffs, and publishes one campaign report.

```text
DefendingCodeJob
  -> DefenseThreatModel
  -> N DefenseReviewArea
  -> N scanners -> DefenseCandidateFinding* + N DefenseScanResult
  -> scan barrier -> K DefenseVerificationAssignment
  -> K per-document triggers -> K independent verifier requests
  -> K DefenseFindingVerdict + K DefenseVerificationCompletion
  -> completion barrier
  -> one triage reducer
       -> confirmed DefendingFinding*
       -> M DefensePatchAssignment (M >= 1; a no-findings sentinel closes zero work)
  -> M patch authors -> M DefensePatchCandidate
  -> M isolated reviewers -> M DefensePatchReview
  -> one report barrier -> DefenseReport
```

Every intermediate artifact is a typed DefraDB document correlated by
`run_id`. Agents never receive `defra_query`: each read tool is bound to one
collection, a fixed projection, and a runtime-filled `run_id`; each write tool
is bound to one collection and an explicit field allowlist.

The current datastore surface supports bounded creates and reads, not bounded
updates, while event edges are create-only. State changes are therefore
append-only facts (`CandidateFinding -> FindingVerdict -> DefendingFinding`,
and `PatchCandidate -> PatchReview`) rather than in-place mutations. This
keeps the full audit history in the graph and avoids introducing free-form
GraphQL merely to simulate status updates.

## Safety boundary

This is the reference harness's **static mode**. Threat-model, planning,
scanning and verifier agents receive native LSP plus an unrestricted shell so
they can use `rust-analyzer`, `rg`, and Git history rather than depending on a
single file reader. Their shell network mode is enabled, their file tools are
read-only, and their prompts prohibit source edits, dependency installation,
builds, tests, and target execution. Run this pack only against an authorized,
trusted checkout and network environment.

Patch authors and isolated patch reviewers receive read-only file and LSP
tools but no shell. They emit unified diffs into
`DefensePatchCandidate.diff`; they do not modify the checkout. The report
stage can only use collection-bound graph tools. Findings, source excerpts,
command output, and diffs are treated as untrusted evidence by downstream
prompts, not as instructions. Network access is available only to the four
inspection stages with unrestricted shell.

The execution-verified C/C++ pipeline still needs the harness's two-container
find/grade trust boundary and gVisor setup. It should not be emulated by
granting this pack unrestricted bash.

## Run

```bash
GENTS_DEFENDING_ROOT=/path/to/repository \
  gents demo run defending-code
```

From this repository, the Make target exposes the same controls:

```bash
make defend \
  DEFENDING_ROOT=/path/to/repository \
  DEFENDING_ENDPOINT=http://100.73.235.38:8000/v1 \
  DEFENDING_MODEL=GLM-5.2 \
  DEFENDING_MAX_CONCURRENT=8
```

While the runtime is active, launch the live document-graph visualizer in a
second terminal:

```bash
make defend-page
```

It opens `http://127.0.0.1:19194/?pack=defending`, proxies the runtime on
`DEFENDING_PORT` (19193 by default), and shows both fan-outs, ledger counts,
per-request token totals, interpolated prompts, typed documents, and tool-call
details. The page is read-only and does not seed or mutate the campaign.

Useful controls:

```bash
export GENTS_DEFENDING_ENDPOINT=http://127.0.0.1:8000/v1
export GENTS_DEFENDING_MODEL=GLM-5.2
export GENTS_DEFENDING_MIN_AREAS=4
export GENTS_DEFENDING_MAX_AREAS=10
export GENTS_DEFENDING_MAX_CONCURRENT=8
export GENTS_DEFENDING_CONTEXT_WINDOW=262144
export GENTS_DEFENDING_COMPACTION_THRESHOLD=0.762939453125 # 200,000 tokens
export GENTS_DEFENDING_PROMPT='Prioritize authorization and data-integrity boundaries.'
```

The verification work ledger makes the campaign a DAG containing another DAG:
the scan barrier writes one assignment document per candidate, a per-document
event trigger creates each isolated verifier request, and each verifier writes
one typed verdict followed by one completion document. A per-group completion
barrier invokes the small final triage
reducer, which joins the closed ledger into routing and patch fan-out. No model
calls `spawn_subagent`; DefraDB documents and event triggers own the fan-out,
counting, retries, and audit trail.

The runner verifies the closed review-area/result ledger, exact
candidate-to-verdict coverage, balanced confirmed/refuted counts, the single
final report, stage tool contracts, and signed request provenance. Results and
all four trace projections land under `runs/<job_id>/`.

## Upstream lineage

Prompt structure and workflow principles are adapted from
`anthropics/defending-code-reference-harness` (Apache-2.0): map before scan,
partition by threat-model focus area, keep discovery permissive, make
verification adversarial, derive severity from preconditions, hunt patch
variants, isolate patch review from finder rationale, and treat target-derived
text as untrusted data. The upstream detection-and-response track is a
different workload over telemetry and is intentionally not folded into this
source-review pack.

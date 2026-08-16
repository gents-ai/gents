# Code-review fan-out/fan-in pack

This pack reviews a Gents pull-request diff with a real four-stage,
model-backed document graph:

```text
ReviewJob -> GLM pre-scan + lens planning
          -> N x ReviewArea -> N x DeepSeek V4 specialized scanners
                            -> CandidateFinding* + N x ScanResult
                            -> GLM adversarial verifier
                            -> N x FindingVerdict + VerificationSummary
                            -> GLM final triage
                            -> confirmed Finding* + TriageReport
```

All graph rows and requests carry the same `run_id`. Recon runs Cargo check and
Clippy, detects the technologies touched by the merge-base diff, chooses the
adequate number of distinct review lenses within configurable bounds (four to
twelve by default), and stamps one closed `expected_total`. This produces a
parallel scanner swarm selected by detected review lens rather than by directory.
Scanners report only evidenced
Critical, Major, or concrete Cleanup candidates and each writes exactly one
sentinel. The verification trigger groups the complete sentinel set
without a counting inference call, fires once with deterministic `group.docs`
ordering, and writes exactly one confirmed/refuted verdict for every candidate.
Its expected cardinality comes from recon's immutable output, so a slow scanner
can delay verification but can never burn the durable group marker with a
partial ledger.
Final triage consumes the closed ledger and publishes only confirmed findings.

The scan write tools hide `run_id` and `expected_total` from model input.
`fill: correlation` stamps the run, while
`fill: {"source_field":"expected_total"}` copies the immutable source snapshot
persisted on the scan request. The pack declares a `write` principal ceiling.
Recon, scanners, and verification receive read-only file tools, the native
`lsp` tool, unrestricted bash, network access for read-only `gh`/dependency
inspection, and background process tools rooted initially at
`${GENTS_REVIEW_ROOT:-.}`. Their prompts prohibit edits and external mutations:
the broad shell exists for Cargo, targeted tests, repository history, PR
metadata/diffs/checks, and dependency API evidence. Recon owns the full-workspace
check and Clippy baseline; each scanner gets its evidence packet and may inspect
full changed files, semantic callsites, and targeted tests. Triage has no shell
or network access. DefraDB reads and writes remain stage-specific.

The verify-to-triage edge intentionally is not a second group barrier. A single
verifier owns the complete candidate set, writes every `FindingVerdict`, and
then writes `VerificationSummary` as its final tool call. That completion record
triggers triage per document. The runner independently checks the exact
candidate-to-verdict bijection and summary count balance, so a verifier that
violates the write-last contract fails acceptance instead of publishing a
plausible but incomplete review.

The default uses `GLM-5.2` on workstation-2 for recon, verification, and final
triage, with DeepSeek V4 Flash on workstation-1 for the parallel
scanner swarm. Both endpoints are OpenAI-compatible vLLM services.

```bash
export GENTS_REVIEW_COORDINATOR_ENDPOINT=http://100.87.27.25:8000/v1
export GENTS_REVIEW_COORDINATOR_MODEL=GLM-5.2
export GENTS_REVIEW_REVIEWER_ENDPOINT=http://100.73.235.38:8000/v1
export GENTS_REVIEW_REVIEWER_MODEL=d4f
```

Review agents are configured as long-running workers: the one-million-turn
default is a practical removal of the runtime's required integer turn ceiling,
stage and runner deadlines default to 24 hours, and every stage uses automatic
strip-then-summarize compaction with access to its live context budget. The
default context window is 256K with a 64K per-turn output reserve. Override
`REVIEW_CONTEXT_WINDOW` and `REVIEW_MAX_OUTPUT_TOKENS` when a serving endpoint
requires different limits. Recon is
instructed to release parallel work promptly, but it has no tool-call budget.
Scheduled inference retries transport failures for the full 24-hour review
window by default, using a 5s/30s/120s backoff ladder with the final delay
repeated. This lets a retained review survive a serving-process restart instead
of losing an otherwise complete fan-out.

The review policy is intentionally Gents-specific. It follows the formal-model
and conformance boundary for modeled semantics, treats DefraDB as the complete
database control plane, and requires reviewers to inspect the pinned DefraDB
features/APIs before proposing a parallel capability. It also checks for concrete
duplicate pathways and removable comments or documentation that repeat code,
implementation history, or another canonical source. Rationale, invariants,
safety arguments, operator contracts, and formal-design records remain valuable.

Run the checked-out branch against `origin/main` from the repository root:

```bash
make review
```

Common overrides:

```bash
make review REVIEW_BASE=main REVIEW_HEAD=my-pr-branch
make review REVIEW_PROMPT='Pay special attention to trigger recovery and ACP'
make review REVIEW_LENSES=8
make review REVIEW_PR=123 REVIEW_MIN_LENSES=6 REVIEW_MAX_LENSES=16 REVIEW_KEEP_HOME=1 REVIEW_JOB_ID=pr-123
make review REVIEW_CONTEXT_WINDOW=262144 REVIEW_MAX_OUTPUT_TOKENS=65536
```

Live response snapshots are batched every five seconds by default so parallel
reviewers do not turn token streaming into database write pressure. Override
that cadence with `REVIEW_STREAM_BATCH_MS` when needed; terminal and tool-call
boundaries still flush immediately.

`make review` builds and runs the workspace CLI, roots file and LSP tools at
`REVIEW_ROOT` (the current repository by default), and fails before inference if
the base or head ref is invalid. Reviewer stages also receive the intentionally
unrestricted bash tool described above. The endpoint/model environment variables
shown above retarget either role without editing the pack.

By default, the Make target asks installed `gh` for the pull request associated
with the current branch. If none exists, it reviews the local ref diff without
PR metadata. `REVIEW_PR` may explicitly be a PR number, URL, or an empty value.
The review stages use `gh` only for reads (`pr view`, `pr diff`, `pr checks`, and
GET requests). The local merge-base diff remains the finding boundary, and recon
checks that its head matches the remote PR before using local build results as
evidence. Long Cargo commands use Gents' `spawn_process`/process-inspection tools
instead of shell `&`.

Every run is written under `demo/code-review/runs/<job-id>/`. `results.json`
contains the final `TriageReport` and confirmed `Finding` rows; `meta.json` and
`projections/` retain the graph evidence and adapter projections.

The runner's deterministic acceptance checks require:

- all `ReviewArea` rows for the run to agree on `expected_total`, equal to recon's chosen and actual count;
- exactly that many tagged `ScanResult` rows with the same immutable count;
- exactly one `AgentRequest` for `(review-verify, event, run_id)`;
- one unique `FindingVerdict` for every candidate, with no missing or extra ids;
- exactly one count-balanced `VerificationSummary`;
- exactly one `AgentRequest` for `(review-triage, event, run_id)`;
- exactly one tagged `TriageReport`;
- verification and report counts both match the durable verdict ledger;
- every promoted `Finding` has `verdict: confirmed` and non-empty fresh evidence.

Finding quality remains model-dependent; the runner enforces graph shape,
durable outputs, and a count-balanced verification ledger.

Recon's `write_review_area` obligation also names `expected_total` as its
dynamic count field. The runtime therefore keeps recon active until the durable
completed writes contain the exact declared closed set; disagreement or an
overfull set fails closed before the scanner fanout can be treated as complete.

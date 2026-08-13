# Code-review fan-out/fan-in pack

This pack reviews the current Rust repository with a real four-stage,
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
Clippy, detects the technologies touched by the diff, chooses exactly four distinct
review lenses, and stamps one closed `expected_total`. This produces four
concurrent scanner agents, selected by detected review lens rather than by
directory. Scanners report only evidenced Critical/Major candidates and each
writes exactly one sentinel. The verification trigger groups all four sentinels
without a counting inference call, fires once with deterministic `group.docs`
ordering, and writes exactly one confirmed/refuted verdict for every candidate.
Its timeout floor is also four, so a slow scanner can delay verification but can
never burn the durable group marker with a partial ledger. Final triage consumes
the closed ledger and publishes only confirmed findings.

The scan write tools hide `run_id` and `expected_total` from model input.
`fill: correlation` stamps the run, while
`fill: {"source_field":"expected_total"}` copies the immutable source snapshot
persisted on the scan request. The pack declares a `write` principal ceiling.
Recon receives read-only file tools plus a network-disabled, workspace-write
shell rooted at `${GENTS_REVIEW_ROOT:-.}` so Cargo and Git can produce the
deterministic baseline. Verification receives read-only file/search tools, its
bounded CandidateFinding query, and schema-generated verdict tools; triage
receives read-only files, its FindingVerdict query, and report tools. Neither
later stage has a shell or network access. Scanners receive only their evidence
packet and schema-generated write tools. DefraDB reads and writes remain
stage-specific.

The verify-to-triage edge intentionally is not a second group barrier. A single
verifier owns the complete candidate set, writes every `FindingVerdict`, and
then writes `VerificationSummary` as its final tool call. That completion record
triggers triage per document. The runner independently checks the exact
candidate-to-verdict bijection and summary count balance, so a verifier that
violates the write-last contract fails acceptance instead of publishing a
plausible but incomplete review.

The default uses `GLM-5.2` on workstation-2 for recon, verification, and final
triage, with DeepSeek V4 Flash on workstation-1 for the four-request parallel
scanner swarm. Both endpoints are OpenAI-compatible vLLM services.

```bash
export GENTS_REVIEW_COORDINATOR_ENDPOINT=http://100.87.27.25:8000/v1
export GENTS_REVIEW_COORDINATOR_MODEL=GLM-5.2
export GENTS_REVIEW_REVIEWER_ENDPOINT=http://100.73.235.38:8000/v1
export GENTS_REVIEW_REVIEWER_MODEL=d4f
```

Run it from the repository root:

```bash
gents config validate --root demo/code-review
gents demo run code-review --prompt "Review for correctness and durability"
```

The runner's deterministic acceptance checks require:

- all `ReviewArea` rows for the run to agree on `expected_total`, equal to their actual count;
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

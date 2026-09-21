# Eval core contract design (issue #1515, sub-project 1)

Status: design, approved section by section on 2026-09-21. Baseline: `main` at `0deb7659c`.
Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`, which defines the vocabulary used here.

## Goal

Define the documents, the outcome vocabulary, the scoring rules and the protection rules that every
eval producer and consumer shares. This spec contains no execution: running trials is spec 2, checks
and the pack format are spec 3, reporting and the CLI are spec 4.

## Non-goals

- Executing trials, provisioning homes, or running checks.
- The check registry's contents and the on-disk pack layout for definitions.
- Report rendering, run comparison UX, and CLI commands.
- Custom executable graders, model judges in the acceptance tier, and subjective grading.
- A lifecycle for runs or trials. These documents record facts.

## 1. The four documents

All four collections are `@branchable`. The directive is irreversible after a schema is created, costs
one extra collection-level commit block per write, and is the precondition for branchable sync and for
collection-scoped ACP reads. Omitting it would permanently cap both.

### 1.1 `EvalDefinition`: a configuration document, carried by packs

```graphql
type EvalDefinition @branchable
    @index(fields: ["agent_did", "definition_id"], unique: true) {
    agent_did: String @index
    definition_id: String
    comparability_version: Int
    title: String
    subject: JSON
    fixtures: JSON
    cases: JSON
    updated_at: String
    tags: [String]
}
```

- `updated_at` is written by the mutation owner and stripped from the identity digest, as for every
  apply-controlled collection. `mint_recreate_identity` depends on it: without it, removing a
  definition and reinstalling byte-identical content would regenerate a tombstoned docID.

- `subject` declares what the definition evaluates: `{kind: "behavior", inference_slots: [...]}`.
- `fixtures` lists pack-asset digests, input documents and schemas to install in a trial.
- `cases` is `[{case_id, split, reducer, stages: [{stage_id, prompt, deadline_secs, checks: [{check,
  params, tier, weight}]}]}]`.
- It joins the `Collection` enum and gets a `PackConfig` root, so it inherits content digests, pack
  provenance tags, install preview with `--digest`, and cleanup.
- Identity is `(definition_id, comparability_version, desired_state_document_digest)`. The digest is
  the integrity anchor. The author bumps `comparability_version` when cases, checks, reducers or judge
  settings change, and the harness refuses to compare runs across different values. One digest alone
  would change for a README fix, which is not a comparability event.
- Cases are embedded. In v1, split protection comes from construction, not from per-document ACP. If
  per-split ACP grants are ever needed, moving cases into their own collection is an additive change.
- It is listed in `LOCAL_AUDIT_COLLECTION_NAMES` and is absent from every P2P collection list. It
  holds acceptance checks and held-out case bodies, and the desktop's bulk subscription would
  otherwise include it.
- An empty `tags` list is written as `null`, per the repository rule.

### 1.2 `EvalRun`: a runtime document, frozen at creation

```graphql
type EvalRun @branchable
    @index(fields: ["owner_agent_did", "run_id"], unique: true) {
    run_id: String @immutable
    owner_agent_did: String @index @immutable
    evaluator_did: String @immutable
    origin: JSON @immutable
    created_at: String @index @immutable
    invalidated: JSON
}
```

`origin` freezes: the definition reference (`definition_id`, `comparability_version`, digest); the
split and the selected `case_id`s; the cells as `{cell_id, label, subject: {pack_digest, behavior_id},
inference binding}`; `trials_per_case`; `seed_base`; deadlines and concurrency;
`denominator_policy`; `taxonomy_version`; `max_infra_retries`; the check-registry version; the harness
source commit and dirty flag; and a `purpose` of `"eval"` or `"optimization:<job_id>"`.

`invalidated` is the only mutable field: `null`, or `{at, by, reason}`. There is no status field.

### 1.3 `EvalTrial`: identity at provisioning, one write-once completion

```graphql
type EvalTrial @branchable
    @index(fields: ["owner_agent_did", "trial_id"], unique: true) {
    trial_id: String @immutable
    owner_agent_did: String @index @immutable
    run_id: String @index @immutable
    cell_id: String @immutable
    case_id: String @immutable
    trial_index: Int @immutable
    attempt: Int @immutable
    trial_agent_did: String @immutable
    session_id: String @immutable
    seed: Int @immutable
    home_hint: String
    created_at: String @immutable
    completion: JSON
}
```

- The durable evidence reference is `(trial_agent_did, session_id)`. The runner chooses the session,
  and the trial's evidence is everything in that session in the trial's own database.
- `completion` is written once: `{ended_at, stages: [{stage_id, request_id, terminal_state,
  failure_kind}], usage, anchor}`. The `anchor` holds the terminal states and the request and
  inference-call counts, so a reader can tell a deleted trial home from a mismatched one.
- A null `completion` is a fact: the runner did not finish this trial. Whether it is running or was
  interrupted derives from the referenced requests' `lifecycle_state`. Resume abandons such a row and
  creates a new one with `attempt + 1`. Reports use the latest completed attempt and list abandoned
  ones as `infrastructure` outcomes.
- `seed` is `seed_base + trial_index`, shared by every cell for the same `(case, trial_index)`. This
  is common random numbers through the existing `InferenceSampling.seed`. It pairs arms and reduces
  variance at no cost. A provider seed does not guarantee deterministic output; it is a variance
  reduction, not a reproducibility claim.
- `home_hint` is a locator only. It is never part of identity, because paths are not portable
  identity data.
- Usage follows the #1512 finding: a missing total is `null`, never zero, and a fresh observation
  timestamp is not fresh usage.

### 1.4 `EvalVerdict`: append-only

```graphql
type EvalVerdict @branchable
    @index(fields: ["owner_agent_did", "verdict_id"], unique: true) {
    verdict_id: String @immutable
    owner_agent_did: String @index @immutable
    run_id: String @index @immutable
    trial_id: String @index @immutable
    stage_id: String @immutable
    check: String @immutable
    check_version: String @immutable
    tier: String @immutable
    outcome_kind: String @immutable
    provider_reason: String @immutable
    score_bp: Int @immutable
    weight: Int @immutable
    raw: JSON @immutable
    feedback: String @immutable
    regrade_of: String @immutable
    created_at: String @immutable
}
```

- Every field is immutable. Re-grading appends a row whose `regrade_of` names the verdict it
  supersedes. Consumers read the latest verdict per `(trial, stage, check)`. A raw verdict is never
  lost.
- `score_bp` is integer basis points in `0..=10000`, null for a non-evidence verdict. Integers were
  chosen over `Float` during planning: the repository has no `@immutable Float` precedent, and
  integers are exact in both Lean and Rust.
- `provider_reason` is `rejected`, `unavailable` or null. The kind-to-class projection needs it.
- `weight` is the check's weight copied from the definition, so a reducer does not re-read the
  definition.
- `raw` holds the check's own verdict verbatim, including `{reason_code, detail}`.
- `feedback` is written only when the case is on the train split. The runner enforces this, not the
  check.
- It is listed in `LOCAL_AUDIT_COLLECTION_NAMES`, because feedback can quote transcripts.
- `EvalRun` and `EvalTrial` hold no message bodies and are not local-audit. They may replicate to the
  operator's own paired clients.

### 1.5 Derived, never stored

- A run's progress and completion: from its trials.
- A trial's state: from its requests' `lifecycle_state`.
- Split exposure: the count of non-invalidated `EvalRun` rows per `(definition_id,
  comparability_version, split)`. A counter field would be forgeable; a count of rows is not.

## 2. Outcome vocabulary

**The rule.** Anything the subject could cause counts against it. Only what it cannot cause is
excluded.

**Totality.** For every check declared on a case, exactly one first verdict row exists once the trial
completes. If a stage never ran, the runner writes a verdict for each of its checks carrying the
stage's failure kind. Denominators are rows, not inferences from gaps.

| `outcome_kind` | Meaning | Class | `score_bp` |
|---|---|---|---|
| `passed` | The check ran and was satisfied | Pass | from the check, up to 10000 |
| `model_acceptance` | The check ran and was not satisfied | Fail | from the check; may be partial |
| `deadline` | The stage ran out of time | Fail | 0 |
| `tool` | A tool execution failed | Fail | 0 |
| `runtime` | The agent runtime failed the request | Fail | 0 |
| `skipped_prerequisite` | An earlier stage of the same case failed | Fail | 0 |
| `provider`, reason `rejected` | A 4xx such as context overflow or a content-policy refusal | Fail | 0 |
| `provider`, reason `unavailable` | A 5xx, a connection failure, rate limiting | NotEvidence | null |
| `infrastructure` | The trial home, runner or host failed; abandoned attempts | NotEvidence | null |
| `provider`, no reason | The evaluator could not tell which | Unknown | null |
| `inconclusive` | The evaluator could not tell | Unknown | null |
| `grader` | The check itself errored | Unknown | null |
| `unknown` | Could not be classified | Unknown | null |

These are the nine kinds in `stages.rs` plus `passed` and `skipped_prerequisite`, which #1512 reports
already emit. `provider` gains a reason split, because a candidate prompt can cause a context
overflow. `tool` and `runtime` are failures: a candidate prompt can drive the runtime into those
paths, the shared seed exposes both arms to the same conditions, and the optimization policy's
asymmetry cap catches the remainder. A non-evidence verdict has a null score, so nobody can average a
fabricated zero.

The table is versioned as `taxonomy_version`, frozen in each run's `origin`, and embedded in reports.
#1512's structured inconclusive reasons, such as `command_attribution_uncertain`, become
`raw.reason_code` values.

## 3. Scoring

- Only acceptance-tier checks enter a case's score. Development-tier checks are reported separately
  and may carry feedback. A model-authored check can never move a decision.
- Each case declares a reducer over its acceptance checks: `weighted_mean` (the default; partial
  credit buys statistical power), `all` (1 only when every check passes), or `last_stage`.
- A case-trial takes the class of its worst acceptance verdict: NotEvidence if any verdict is
  NotEvidence; otherwise Unknown if any is Unknown; otherwise it is scored.
- A case's score is the mean over its evidence trials. A run's headline is the mean over cases,
  equally weighted. The case is the clustering unit for every error bar.

### Denominator policy

Frozen in `EvalRun.origin`. V1 defines one, `exclude_not_evidence_v1`:

- NotEvidence is excluded, and its count is always shown.
- Unknown is its own bucket in reports. In a single-run headline it counts as failure. In a paired
  comparison it is imputed worst-case: it fails the candidate and passes the baseline.
- A pair is `(case, trial_index)` across two cells, joined by the shared seed. A pair counts only when
  both sides are evidence. Dropped pairs are counted per cell.
- The runner may retry a NotEvidence trial as a new attempt, up to `max_infra_retries`. It never
  retries a Fail or an Unknown. A retry cannot erase a failure.

### Re-grading

Evidence stays in retained trial homes and verdicts are append-only, so an improved check can be run
over an old run. It appends verdicts with `regrade_of` set. Two runs compare only when their latest
verdicts share check versions; re-grading the older run is how they get there.

## 4. Protection

Four threats, each closed by a different mechanism.

| Threat | Closed by |
|---|---|
| T1. The subject, inside a trial, reads or edits graders or held-out material | The materialization invariant |
| T2. An optimizer learns validation or held-out answers, or overfits by repeated queries | The proposer sees only train evidence, by construction; derived exposure counts; held-out touched once per job |
| T3. A model-authored eval weakens assertions | Tiers, and who may author which tier |
| T4. Forged run or verdict rows fake evidence or reset a budget | Eval collections are writable only by the runner, as the owner; budgets are row counts |

### T1: what a trial is seeded with

- The subject pack's documents, installed onto the trial's fresh DID, with its inference binding.
- The case's fixtures: workspace files, input documents, schemas.
- Stage prompts, one at a time, each submitted as an `AgentRequest` when its stage starts. A later
  stage's prompt does not exist in the trial before then.
- Requests carry the evaluator DID as requester. The session is labelled only with the opaque
  `trial_id`.
- Never present: check names and parameters, expected values, rubrics, tier and split labels,
  `case_id` values, other cases.

This is tested, not proved. A scripted definition plants canary strings in every protected field, a
run executes, and the test scans every document and workspace file in the trial home for them.

### T2: splits

- Each case declares `train`, `validation` or `held_out`. A run evaluates exactly one split.
- Only train verdicts carry feedback.
- Exposure is derived (section 1.5) and always shown. A heavily queried split is labelled worn. The
  remedy is new cases under a new `comparability_version`.
- An operator may run the held-out split directly. It is permitted, recorded, and counts as exposure.

### T3: tiers and authorship

- The acceptance tier is deterministic checks only in v1. An `llm_judge` check is development-tier.
- `EvalDefinition` is not a `SelfConfigTarget` in v1. Definitions arrive by operator pack install with
  `--digest`. When the configurator authors definitions later, that write path forces every check to
  the development tier.

### T4: ownership

- `EvalRun`, `EvalTrial` and `EvalVerdict` are written only by the runner, under the owner DID. The
  evaluator DID is recorded on the run.
- A `DatastoreToolSurface` may never name an eval collection or `OptimizationJob`. No agent in the
  launching home, including the live version of a behavior being optimized, can read or write them
  through its datastore tools.
- ACP on `EvalDefinition` and `EvalVerdict`, reusing the `ProjectionAcpBinding` shape, is defense in
  depth. No guarantee in this spec depends on it (see the umbrella's open items).

### Invalidation

`EvalRun.invalidated` is set by the operator with a reason. Reports and comparisons exclude
invalidated runs and count the exclusions. A consumer that made a decision on a run must be able to
show that the run was later invalidated; spec 5 does this for optimization jobs.

### What v1 cannot do

The runner provisions each trial home, so it is trusted relative to that trial. The evaluator DID is
provenance until trials run as separate processes behind an endpoint (spec 2b), which is when
read-only access can be enforced. T1 is closed by construction plus the canary test, not by
authorization.

## 5. Lean

- `Proofs/ConfigDocuments.lean` gains `EvalDefinition`. A Rust test cross-checks that catalog, so the
  Lean change comes first.
- The kind-to-class projection extends the imputation function in `Proofs/Optimization.lean` and is
  emitted through the existing vocabulary conformance contract, so Rust cannot add a kind the model
  does not know. Theorems: the projection is total; no kind marked subject-causable maps to
  NotEvidence; case-class reduction is monotone, meaning a worse verdict never improves a case's
  class.
- The three runtime documents model no transitions, because they have none.

## 6. Testing

- Unit: digest and identity; the kind-to-class projection against the Lean vocabulary; each reducer;
  case-class reduction; pairing by seed, including dropped pairs and worst-case imputation; exposure
  counting with invalidated runs excluded.
- Schema: the `gents-schemas`, `gents-protocol` and `gents-migration` catalog tests, with new baseline
  pins from `canonical_catalog_pins_for_authoring`; a test that the four collections are
  `@branchable`; a test that `EvalDefinition` and `EvalVerdict` are local-audit and that no eval
  collection appears in a P2P list.
- Validation: a `DatastoreToolSurface` naming an eval collection or `OptimizationJob` is rejected; a
  verdict with feedback on a non-train case is rejected at the contract layer.
- The canary test belongs to spec 2, because it needs a runner.

## 7. Open items

- The ACP disagreement and the reserved-collection question, both recorded in the umbrella.
- Resolved during planning: `score` is `score_bp: Int`, not `Float`.
- Implementation plan: `docs/superpowers/plans/2026-09-21-eval-core-contract.md`.

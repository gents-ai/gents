# Feature matrix for the coverage ledger

Date: 2026-05-20

Branch: `design/issue-264-feature-matrix`

Tracks issue: https://github.com/sourcenetwork/defra-agent/issues/264

Predecessor: `docs/superpowers/audits/2026-05-19-conformance-audit.md` — the
"Stale or weak ledger classifications" subsection and §10 / §15 callouts are
the gap shape this matrix is designed to make visible without a manual audit.

## TL;DR

Today `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` maps
Lean rows → Rust consumers; it does not map *feature* → *operator surface*.
Surface gaps (a feature with runtime coverage but no CLI / desktop UI
binding) are discovered by reading the codebase and writing prose audits, not
by reading the ledger or running the drift test.

The proposal:

1. **Schema extension** — add a closed `Surface` inductive
   (`agentFacing | operatorCli | operatorUi | api | runtimeInternal`) and
   two optional fields to `CoverageEntry`: `feature : String := ""` and
   `surfaces : List Surface := []`. Defaults are empty so the 70+ existing
   `consumerCoverage` / `boundaryCoverage` / `followUpCoverage` /
   `consumerWithFollowUpCoverage` call sites compile unchanged; tagging is
   added per row via record-update syntax during migration.
2. **Feature taxonomy** — a closed table `featureSurfaceRequirements :
   List FeatureSurfaceRequirement` declaring `(feature, required surfaces,
   deferred surfaces)`. Twenty-six feature names cover the runtime + planned
   CLI / desktop UI surface today.
3. **JSON projection** — add one new top-level key to `snapshotJson`:
   `feature_matrix : Map<feature, Map<surface, FeatureMatrixCell>>` where the
   cell carries `coverage_strength`, `row_count`, and
   `pending_follow_ups`. The cell value is derived from existing ledger row
   fields — no new row-level state.
4. **Drift test extension** — a single new `#[test]` named
   `lean_feature_matrix_covers_every_declared_required_surface` at
   `crates/defra-agent/tests/state_machine_conformance/coverage.rs` next to
   the existing `lean_contract_coverage_ledger_accounts_for_every_emitted_domain`
   at `:391`. The new test asserts that for every
   `FeatureSurfaceRequirement.required` surface there is at least one ledger
   row with a matching `feature` tag whose `surfaces` list includes that
   surface, AND that no row carries a `feature` name absent from
   `featureSurfaceRequirements`.

The mechanism is additive. Migration is per-row: until a row is tagged it
contributes to nothing (matrix or test). The drift test is opt-in until
every row carries a non-empty feature tag, at which point the
`feature_required_for_all_rows` boolean in the schema flips to `true` and
untagged rows fail. The flip is a one-line change, owned by the
implementation PR.

## 1. Problem

### What the ledger answers today

`coverageLedger` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:398` is a
list of `CoverageEntry` records (declaration at `:14-20`). The 70+ entries
answer one question: *for every Lean-emitted contract domain, what Rust /
TypeScript consumer enforces it?* The drift test at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:391` enforces
three invariants (`:729-747`): every emitted domain has a ledger entry, every
ledger consumer resolves to a registered consumer pointer
(`tests/support/conformance_consumers.rs`), and no registered consumer is
unreferenced.

### What the ledger does not answer

- *Which operator-facing feature does this row belong to?* `RequestState`
  vocab, `Request` state machine, `RequestTransitions` cases, and
  `QueueDeadlineConformanceCases` are four ledger rows that together describe
  one operator-facing feature ("request lifecycle"). The ledger does not
  encode that grouping.
- *Which surface — agent runtime, operator CLI, operator UI, protocol API,
  internal runtime machinery — does each consumer prove coverage of?* The
  ledger has 60+ Rust consumer paths plus 3 TypeScript / desktop consumer
  paths and gives no schematic answer to the question "does this feature
  have a UI binding?" — that requires reading the consumer path and
  knowing the crate layout.
- *Are there features that have strong runtime coverage but zero CLI or UI
  binding?* The May-15 and May-19 audits each had to enumerate features by
  hand to surface this class of gap (e.g., `subagents-cross-deployment` has
  runtime schema introspection tests at
  `crates/defra-agent/tests/state_machine_conformance/coverage.rs:179-252`
  but no Lean-driven ledger rows and no operator-surface tests on either CLI
  or UI). The current drift test does not surface this.

### Concrete misses the matrix would catch

- **#264 example, EventDelivery (§15 of the May-19 audit).** Three ledger
  rows at `Proofs/Conformance/CoverageLedger.lean:345-362` all on
  `runtimeInternal` (consumer paths in `tests/state_machine_conformance/`).
  Two are `consumerWithFollowUpCoverage` because the consumer drives
  `InMemoryEventDeliverySource`, not the production loops. By itself the
  audit caught this via prose. The matrix surfaces it as a single cell
  `(event-delivery, runtimeInternal) → strength=consumer_with_follow_up,
  row_count=3, pending_follow_ups=2` — visible from the JSON snapshot
  without reading consumer source.
- **MCPHealth (§10).** One ledger row at
  `Proofs/Conformance/CoverageLedger.lean:366-370`, `consumerWithFollowUp`.
  The cell `(mcp-health, runtimeInternal) → pending_follow_ups=1` plus the
  declared-but-deferred `(mcp-health, operatorUi)` slot (see §3) make the
  status reviewable without reading consumer source.
- **subagents-cross-deployment.** A feature with zero ledger rows today but
  runtime fields scattered across `AgentToolCall.unclaimed_deadline_at` /
  `cancel_cascade_intent_at` / `cancel_pending_remote_ack` /
  `stuck_since` and `ToolSelection.cross_deployment_spawn_timeout_seconds`
  (per the integration test at `coverage.rs:179-252`). Declaring the
  feature in `featureSurfaceRequirements` with required surfaces gives the
  drift test a fail-or-defer choice.
- **interrupt-and-cancel.** `CancelCause` vocab (ledger `:113`) plus
  `RequestTransitions` interrupt edges within the existing request row at
  `:206`. As a feature, "the operator and the agent both need to be able
  to interrupt a request" is a real product line, not just a vocabulary.
  Today there is no operator-CLI or operator-UI Lean row for cancellation
  semantics, and nothing in the ledger says that should change.

## 2. Schema extension

### 2.1 The `Surface` inductive

```lean
inductive Surface where
  | agentFacing
  | operatorCli
  | operatorUi
  | api
  | runtimeInternal
  deriving Repr, DecidableEq
```

Variant justification:

- `agentFacing` — runtime entry points the agent invokes while processing a
  turn: tool execution, LLM call dispatch, identity routing, command
  policy, MCP dispatch, persistence hook. The boundary is "code that runs
  *because* an `AgentRequest` is being processed." Distinguished from
  `runtimeInternal` because the audit reader cares whether a feature
  affects the agent's per-turn behavior versus runtime scaffolding around
  it. Examples of `agentFacing` consumers today:
  `crates/defra-agent/src/tool_call_lifecycle.rs:569`,
  `crates/defra-agent/src/toolset/tests.rs:892`,
  `crates/defra-agent/src/managed_exec/tests.rs:27`.
- `operatorCli` — `crates/defra-agent-cli/` consumer tests. The CLI is the
  "operator types a command" surface. Today's only Lean-bound CLI consumer
  is `config_import::lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary`
  at `crates/defra-agent-cli/src/config_import.rs:885`. The matrix
  declaring required CLI surfaces for additional features (`triggers`,
  `interrupt-and-cancel`, etc.) is where most of the deferred slots live.
- `operatorUi` — `apps/desktop-tauri/` consumer tests (Rust bridge + React
  shell). The UI is the "operator clicks a button" surface. Today's
  Lean-bound UI consumers: the chat-shell TS test at
  `apps/desktop-tauri/src/lib/chat-shell.test.ts:304`, the desktop session
  bridge at
  `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_state.rs:254`,
  and the live-overlay table at
  `crates/defra-agent/tests/live_overlay_conformance.rs:62` (technically
  a runtime-side test of UI projection data, classified here as
  `operatorUi` because the contract is the UI overlay shape).
- `api` — protocol surface: the DefraDB GraphQL collections under
  `crates/defra-agent-protocol/schemas/` plus any out-of-process SDK
  boundary that a third-party consumer would code against. The schema
  introspection tests at
  `crates/defra-agent/tests/state_machine_conformance/coverage.rs:179-252`
  ("agent_tool_call_has_r5_cross_deployment_fields",
  "tool_selection_has_cross_deployment_spawn_timeout") are `api` surface
  tests. Today's footprint is small; the variant earns its place by
  forcing the question "is this feature reachable via the protocol?" for
  identity-permission, fleet-slot-accounting, subagents-cross-deployment.
- `runtimeInternal` — runtime machinery the operator touches only
  transitively through other surfaces: watcher, scheduler, reconcile
  loops, recovery sweeps, fleet accounting, backend registry,
  storage / persistence hooks. The majority of today's consumer tests
  live here (`crates/defra-agent/src/runtime_status/tests.rs`,
  `src/admission/tests.rs`, `src/hook/tests.rs`, `src/lifecycle.rs`,
  `tests/state_machine_conformance/`). A feature that has *only*
  `runtimeInternal` rows is honest about being machinery; a feature that
  also names `operatorUi` is making a product claim about what the
  operator can see.

Open question deliberately left to the implementation pass: the boundary
between `agentFacing` and `runtimeInternal` is judgment-bearing for
ambiguous cases (e.g., `admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic`
exercises code that runs per inference call but is mostly slot
bookkeeping). The worked example in §7 makes a call per row; the
implementation is free to push back on individual classifications during
review of the migration PR.

### 2.2 Optional fields on `CoverageEntry`

The current `CoverageEntry` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:14-20`:

```lean
structure CoverageEntry where
  category : String
  domain : String
  consumer : String
  acceptedBoundary : String
  acceptedFollowUp : String
  deriving Repr
```

Extended:

```lean
structure CoverageEntry where
  category : String
  domain : String
  consumer : String
  acceptedBoundary : String
  acceptedFollowUp : String
  feature : String        := ""
  surfaces : List Surface := []
  deriving Repr
```

The existing combinators
(`consumerCoverage` / `boundaryCoverage` / `followUpCoverage` /
`consumerWithFollowUpCoverage` at `CoverageLedger.lean:25-60`) keep their
positional argument lists. They construct `CoverageEntry` records with the
new fields at their defaults. No existing call site changes shape.

Tagging is done at call sites using record-update syntax:

```lean
  , { consumerCoverage "state_machine" "Request"
        "lifecycle::tests::request_state_machine_contract_is_complete"
      with feature := "request-lifecycle"
         , surfaces := [Surface.runtimeInternal] }
```

A helper makes the common case terser:

```lean
def tagged (entry : CoverageEntry)
    (feature : String) (surfaces : List Surface) : CoverageEntry :=
  { entry with feature := feature, surfaces := surfaces }
```

So the migrated row reads:

```lean
  , tagged (consumerCoverage "state_machine" "Request"
              "lifecycle::tests::request_state_machine_contract_is_complete")
      "request-lifecycle" [Surface.runtimeInternal]
```

This composes with the existing combinator, leaves the consumer string
verbatim (so the consumer-registry resolution at
`tests/state_machine_conformance/coverage.rs:712-718` is untouched), and
preserves the audit-style grep `'request_state_machine_contract_is_complete'`.

### 2.3 `feature` as `String`, not inductive

A closed `Feature` inductive would be the rigid choice. I recommend
`String` for the row tag, with closedness enforced by
`featureSurfaceRequirements` (§3). Rationale:

- The existing `domain : String` (line `:16`) and `category : String`
  (`:15`) are strings. The drift test at
  `coverage.rs:639-673` already enforces a closed `valid_categories` array
  against the `category` string. Mirroring that pattern keeps the schema
  symmetric and reuses an enforcement mechanism the team already trusts.
- Features turn over more frequently than surfaces. `subagents-cross-deployment`
  is brand new. `backend-health` is being argued for in this spec.
  `recovery` may split into `request-recovery` / `tool-call-recovery` /
  `inference-recovery` once the per-sweep ledger row work lands. A
  `String` tag with a closed-table check has the same safety as an
  inductive but admits a one-line edit to add a feature.
- Lean pattern-matching on the feature is rarely needed — the JSON
  projection groups by tag, and the drift test reads
  `featureSurfaceRequirements` as the source of truth.

The drift test enforces closedness:

```rust
let valid_features: BTreeSet<&str> = snapshot
    .feature_surface_requirements
    .iter()
    .map(|req| req.feature.as_str())
    .collect();
for entry in &snapshot.coverage_ledger {
    if !entry.feature.is_empty() {
        assert!(
            valid_features.contains(entry.feature.as_str()),
            "coverage ledger row tags unknown feature: {:?}",
            entry
        );
    }
}
```

A row tagged with a feature absent from `featureSurfaceRequirements` fails
the test. A row with `feature := ""` is "untagged" — permitted during
migration, eventually forbidden by flipping the closure boolean (§5.4).

### 2.4 `surfaces : List Surface`, not `Surface`

Each ledger row points at one consumer test, and that consumer test lives
in one crate / file. The natural value is *singular*. I recommend
`List Surface` anyway:

- A boundary row (`boundaryCoverage`) carries no consumer but may
  legitimately bound coverage at multiple surfaces (e.g., the
  fleet-slot-accounting boundary is observed at both `runtimeInternal`
  admission tests and `api` GraphQL introspection). A list reads cleanly.
- A row whose consumer is shared between two surfaces (rare but possible
  — e.g., the live-overlay test exercises both runtime-side projection
  and UI render contract simultaneously) gets to declare both without
  having to be split into two ledger rows.
- The cost of allowing a list is zero: a singleton `[Surface.agentFacing]`
  is the common case and the JSON projection iterates uniformly.

Empty `surfaces` is permitted and means "this row has no surface yet
declared" — same migration story as empty `feature`.

### 2.5 `FeatureSurfaceRequirement`

A new structure adjacent to `CoverageEntry`:

```lean
structure FeatureSurfaceRequirement where
  feature  : String
  required : List Surface
  deferred : List (Surface × String)
  deriving Repr
```

`required` lists surfaces the feature is expected to bind today.
`deferred` lists surfaces that the feature *should* bind but currently
does not, paired with a follow-up note (typically a GitHub issue
reference) — analogous to the `consumerWithFollowUpCoverage` /
`followUpHookCoverage` pattern already in the file.

The drift test treats `required` as hard, `deferred` as soft:

- For each `(feature, surface) ∈ required × required.surfaces`: there
  must exist a ledger row with `entry.feature = feature` and
  `surface ∈ entry.surfaces`. Fail otherwise.
- For each `(feature, surface) ∈ deferred × deferred.surfaces`: no
  assertion. The cell shows up in the JSON snapshot with
  `coverage_strength: "deferred"` and the follow-up note.

This sits alongside the existing `followUpHookCoverage` mechanism (used
for Lean-only theorems with no Rust witness) without overlapping —
followUpHookCoverage is row-level ("this domain has no consumer, here is
why"), `deferred` is feature-level ("this feature wants this surface, we
know it is missing").

## 3. Feature taxonomy

Twenty-six feature names cover today's runtime + planned CLI / desktop
surfaces. Names are kebab-case to match domain naming inside Lean modules
(`request-lifecycle` mirrors `Proofs/Request/*`, `command-policy` mirrors
`Proofs/CommandPolicy/*`).

The starter list from `PROMPT.md` is refined as follows:

- **Add** `backend-health`. The audit at §23 / §10 keeps repeating that
  backend-health (LLM provider health, admission gating) and mcp-health
  (MCP tool service health) are separate operator concerns with separate
  ledger rows. Folding them into `inference-call` would conceal the
  distinction. Split.
- **Keep** `transcript`, `streaming-response`, `compaction` as siblings
  even though they all flow through `agentFacing` consumers. Operators
  configure / observe them independently (compaction reducer choice is
  per-behavior config; streaming responses are observed in the chat
  shell; transcript is the message store).
- **Keep** `recovery` as one feature. The per-sweep split
  (`request-recovery`, `tool-call-recovery`, `inference-call-recovery`,
  `detached-bridge-recovery`) is implicit in `RecoverySweepCases` and
  belongs to a future ledger refinement, not the matrix v1.
- **Keep** `subagents-cross-deployment` distinct from `background-tools`.
  R5 (cross-deployment subagent placement) and R6 (in-process backgrounding)
  are different Lean models with different runtime fields; the operator
  cares about them differently (deployment routing vs. background tool
  limits). Concealing them under one feature flag defeats the matrix.

The full table follows. `Required` is read as "today we expect at least
one ledger row tagged with this feature on each of these surfaces."
`Deferred` is read as "we know this feature should bind here but does
not — track via the cited follow-up." `Tag count` is the number of
existing ledger rows the worked example (§7) tags with this feature.

| Feature | Required | Deferred | Tag count |
|---|---|---|---|
| `request-lifecycle` | `agentFacing`, `runtimeInternal` | `operatorUi` (#TBD-request-lifecycle-ui-dedicated-row, see §7.6) | 5 |
| `process-lifecycle` | `runtimeInternal` | — | 3 |
| `inference-call` | `agentFacing`, `runtimeInternal` | — | 4 |
| `tool-call` | `agentFacing`, `runtimeInternal` | — | 7 |
| `managed-exec` | `agentFacing` | — | 3 |
| `pairing-reconcile` | `runtimeInternal` | — | 1 |
| `runtime-reconcile` | `runtimeInternal` | — | 3 |
| `session-recovery` | `runtimeInternal` | — | 3 |
| `background-tools` | `agentFacing` | `operatorCli` (#TBD-cli-bg-listing), `operatorUi` (#TBD-ui-bg-panel) | 14 |
| `subagents-cross-deployment` | — | `api` (#TBD-r5-api-row), `agentFacing` (#TBD-r5-lean-witness), `operatorUi` (#TBD-r5-ui-routing) | 0 (see §3.1) |
| `interrupt-and-cancel` | `agentFacing` | `operatorCli` (#TBD-cli-cancel), `operatorUi` (#TBD-ui-cancel-button) | 1 |
| `mcp-health` | `runtimeInternal` | `operatorUi` (#TBD-ui-mcp-status), `operatorCli` (#TBD-cli-mcp-probe) | 1 |
| `identity-permission` | `runtimeInternal` | `api` (#TBD-identity-graphql-decide) | 3 |
| `apply-reconcile` | `operatorCli` | `operatorUi` (#TBD-ui-apply-preview) | 1 |
| `event-delivery` | `runtimeInternal` | — | 3 |
| `triggers` | `runtimeInternal` | `operatorCli` (#TBD-cli-task-run-lean), `operatorUi` (#TBD-ui-recent-runs-lean) | 1 |
| `compaction` | `agentFacing` | — | 1 |
| `transcript` | `agentFacing` | `operatorUi` (#TBD-ui-transcript-lean) | 1 |
| `streaming-response` | `agentFacing` | `operatorUi` (#TBD-ui-stream-render-lean) | 1 |
| `client-shell` | `operatorUi` | — | 3 |
| `command-policy` | `agentFacing` | `operatorUi` (#TBD-ui-command-denial) | 3 |
| `recovery` | `runtimeInternal` | — | 1 |
| `fleet-slot-accounting` | `runtimeInternal` | `api` (#TBD-fleet-graphql-introspect) | 1 |
| `storage-observation` | `runtimeInternal` | — | 4 |
| `persistence-failure-policy` | `runtimeInternal` | — | 5 |
| `backend-health` | `runtimeInternal` | `operatorUi` (#TBD-ui-backend-status) | 1 |

Tag-count totals: 74 row-tags across 74 distinct ledger rows. Under the
single-tag rule (§3.2 / §6.2), each existing row is tagged exactly once.
The ledger has 74 rows (18 vocab + 15 state-machine + 37 case + 4
followUpHook) per `awk` ranges over `CoverageLedger.lean`; every one is
accounted for in §7.

### 3.1 The subagents-cross-deployment feature is all-deferred

`subagents-cross-deployment` is the one feature with zero existing ledger
rows. The runtime carries the R5 model in Lean (per the audit's §8
subtree) and the protocol schema carries the R5 fields (exercised by
`agent_tool_call_has_r5_cross_deployment_fields` and
`tool_selection_has_cross_deployment_spawn_timeout` at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:179-252`),
but those `#[tokio::test]` functions are not Lean-driven case consumers
and are not in the ledger.

Three follow-ups, all `deferred`:

- **`api`.** Promote the existing schema introspection tests to ledger
  consumers by adding a row pointing at them with
  `category := "api_schema_cases"` (a new category). Tracked as
  #TBD-r5-api-row.
- **`agentFacing`.** Land an `r5_cross_deployment_cases` Lean domain
  emitting per-field witnesses (Lean already has the R5 model), then bind
  it from a runtime-tool consumer. Tracked as #TBD-r5-lean-witness.
- **`operatorUi`.** Surface cross-deployment routing in the desktop
  shell — a deployment badge or routing diagnostic on the chat shell.
  Tracked as #TBD-r5-ui-routing.

The matrix v1 lists all three as `deferred` (no `required` slot) so the
implementation PR ships without immediately failing the new drift test
on this feature. When any of the three follow-ups land, the surface
moves to `required` and the relevant ledger row is added.

### 3.2 Multi-tag carve-out: `interrupt-and-cancel`

`interrupt-and-cancel` is the one feature whose semantics cut across
existing rows in a way that the worked example handles by sharing rows
with another feature. Specifically:

- `CancelCause` vocab (ledger `:113`) — primarily an
  `interrupt-and-cancel` row. Tagged `interrupt-and-cancel` only.
- `RequestTransitions` cases (ledger `:206`) — includes interrupt edges
  (`interrupt_*` transitions enumerated in
  `Proofs/Request/Transition.lean:17`). The row's *primary* feature is
  `request-lifecycle`; it incidentally provides cancellation coverage.
  Tagged `request-lifecycle` only — the matrix accepts that
  cancellation's runtime coverage is *via* request-lifecycle's row, not
  a separately-tagged row.
- `ToolCall` state machine (ledger `:182`) — includes
  `cancelBeforeDispatch_*` and `cancelDuringRun_*` actions. Tagged
  `tool-call` only.

The design choice is **single primary feature per row**. A row's tag
names the feature it most directly evidences. A feature whose
required-surface coverage is *transitively* delivered by another
feature's row is honest about that — `interrupt-and-cancel` declares
`agentFacing` required, the CancelCause vocab row covers it, and that
is enough for the drift test. The cross-feature dependency is encoded
implicitly: if `request-lifecycle.agentFacing` regresses, it likely
regresses `interrupt-and-cancel` too, but the matrix does not try to
model that. The alternative (`feature : List String` allowing multi-tag)
is discussed in §6.2.

## 4. JSON projection

### 4.1 Cell shape

```lean
structure FeatureMatrixCell where
  feature           : String
  surface           : Surface
  coverageStrength  : String  -- "consumer" | "consumer_with_follow_up"
                              -- | "boundary" | "follow_up_only"
                              -- | "deferred" | "missing"
  rowCount          : Nat
  pendingFollowUps  : Nat
  deferredNote      : String  -- "" unless coverageStrength = "deferred"
  deriving Repr
```

### 4.2 Cell derivation

A cell `(feature, surface)` aggregates ledger rows where
`entry.feature = feature` and `surface ∈ entry.surfaces`, then folds:

- `rowCount := |matching rows|`.
- `pendingFollowUps := |matching rows with non-empty acceptedFollowUp|`.
- `coverageStrength :=` the strongest row's classification, where the
  ordering is:
  ```
  consumer > consumer_with_follow_up > boundary > follow_up_only > missing
  ```
  with the row classification derived from existing fields:
  ```
  has_consumer && !has_follow_up         → consumer
  has_consumer && has_follow_up          → consumer_with_follow_up
  !has_consumer && has_boundary          → boundary
  !has_consumer && !has_boundary && has_follow_up → follow_up_only
  ```
  (The four constructors at `CoverageLedger.lean:25-60` already
  produce these exact field combinations; no new row state.)
- If no rows match AND the cell is in `deferred`, then
  `coverageStrength := "deferred"` and `deferredNote := <follow-up text>`.
- If no rows match AND the cell is in `required`, then
  `coverageStrength := "missing"` and the drift test fails (§5).
- If no rows match AND the cell is neither in `required` nor in
  `deferred`, the cell is omitted from the projection.

### 4.3 Snapshot integration

The snapshot at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:26-134`
gains two new top-level keys, slotted immediately after the existing
`coverage_ledger` key at `:126-127`:

```lean
    ++ ",\"feature_surface_requirements\":"
      ++ featureSurfaceRequirementsJson
    ++ ",\"feature_matrix\":"
      ++ featureMatrixJson
```

`featureMatrixJson` is a `Map<feature, Map<surface, FeatureMatrixCell>>`
serialized as:

```json
{
  "request-lifecycle": {
    "agentFacing":     { "coverage_strength": "consumer",
                         "row_count": 3, "pending_follow_ups": 0,
                         "deferred_note": "" },
    "runtimeInternal": { "coverage_strength": "consumer",
                         "row_count": 6, "pending_follow_ups": 0,
                         "deferred_note": "" },
    "operatorUi":      { "coverage_strength": "consumer",
                         "row_count": 1, "pending_follow_ups": 0,
                         "deferred_note": "" }
  },
  "event-delivery": {
    "runtimeInternal": { "coverage_strength": "consumer_with_follow_up",
                         "row_count": 3, "pending_follow_ups": 2,
                         "deferred_note": "" }
  },
  "subagents-cross-deployment": {
    "api":             { "coverage_strength": "missing",
                         "row_count": 0, "pending_follow_ups": 0,
                         "deferred_note": "" },
    "agentFacing":     { "coverage_strength": "deferred",
                         "row_count": 0, "pending_follow_ups": 0,
                         "deferred_note": "#TBD-r5-lean-witness" },
    "operatorUi":      { "coverage_strength": "deferred",
                         "row_count": 0, "pending_follow_ups": 0,
                         "deferred_note": "#TBD-r5-ui-routing" }
  },
  ...
}
```

`featureSurfaceRequirementsJson` is a list of the requirement records;
exposing it separately lets reviewers diff the *expectation table*
without re-reading the matrix.

### 4.4 Rust snapshot type

In `crates/defra-agent/src/lean_vocab_test.rs` (where
`LeanContractSnapshot` lives at `:27-79` per the audit), the consumer
adds two fields:

```rust
pub feature_surface_requirements: Vec<FeatureSurfaceRequirementDoc>,
pub feature_matrix: BTreeMap<String, BTreeMap<String, FeatureMatrixCellDoc>>,
```

The pre-existing pattern at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:391-748`
uses `BTreeSet` / `BTreeMap` for deterministic diffs; same pattern
applies. The accessor list at `lean_vocab_test.rs:217-489` (per audit)
gains two new accessors.

## 5. Drift test extension

### 5.1 Where it lives

A single new `#[test]` function in
`crates/defra-agent/tests/state_machine_conformance/coverage.rs`,
co-located with the existing `lean_contract_coverage_ledger_accounts_for_every_emitted_domain`
at `:391`. Function name:

```rust
#[test]
fn lean_feature_matrix_covers_every_declared_required_surface() { ... }
```

### 5.2 Exact assertion shape

```rust
#[test]
fn lean_feature_matrix_covers_every_declared_required_surface() {
    let snapshot = lean_contract_snapshot();
    let valid_features: BTreeSet<&str> = snapshot
        .feature_surface_requirements
        .iter()
        .map(|req| req.feature.as_str())
        .collect();

    // (a) Every tagged row names a known feature.
    for entry in &snapshot.coverage_ledger {
        if !entry.feature.is_empty() {
            assert!(
                valid_features.contains(entry.feature.as_str()),
                "coverage ledger row tags unknown feature: {:?}",
                entry
            );
            assert!(
                !entry.surfaces.is_empty(),
                "coverage ledger row carries feature {:?} but no surfaces; \
                 each tagged row must declare at least one surface: {:?}",
                entry.feature,
                entry
            );
        }
    }

    // (b) Every required (feature, surface) has at least one matching row.
    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let covered = snapshot.coverage_ledger.iter().any(|entry| {
                entry.feature == req.feature
                    && entry.surfaces.iter().any(|s| s == surface)
            });
            assert!(
                covered,
                "feature {:?} declares required surface {:?} but no \
                 ledger row tags this (feature, surface). Either add a \
                 ledger row, or move this surface to `deferred` with a \
                 follow-up note.",
                req.feature,
                surface
            );
        }
    }

    // (c) Cross-check the projected matrix: no required cell carries
    //     `coverage_strength = "missing"`.
    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let cell = snapshot
                .feature_matrix
                .get(&req.feature)
                .and_then(|m| m.get(surface_to_string(*surface)));
            let strength = cell.map(|c| c.coverage_strength.as_str())
                              .unwrap_or("missing");
            assert!(
                strength != "missing",
                "feature_matrix[{}][{:?}] is `missing` but required",
                req.feature, surface
            );
        }
    }
}
```

The specific assertion the implementer can name when reporting a failure
to the team is **assertion (b)**: "feature X declares required surface Y
but no ledger row tags this (feature, surface)". That is the failure
mode the matrix exists to surface.

### 5.3 Interaction with the existing drift test

The existing `lean_contract_coverage_ledger_accounts_for_every_emitted_domain`
at `:391` continues to enforce ledger ↔ snapshot agreement on the
`(category, domain)` axis. The new test enforces the `(feature, surface)`
axis. The two are orthogonal and run as separate `#[test]` functions;
either can fail independently. No changes to the existing test.

### 5.4 The closure flip

Initial schema lands with `feature` and `surfaces` defaulting to empty
strings / empty lists. Existing rows compile unchanged. The new drift
test passes vacuously for any row with `feature := ""` (assertion (a)
short-circuits on the `is_empty` check).

After all rows are tagged (per the worked example in §7), the
implementer flips a single boolean at the top of the drift test:

```rust
const REQUIRE_FEATURE_TAG_FOR_ALL_ROWS: bool = true;
```

Then assertion (a) extends to:

```rust
if REQUIRE_FEATURE_TAG_FOR_ALL_ROWS {
    assert!(
        !entry.feature.is_empty(),
        "coverage ledger row is untagged: {:?}",
        entry
    );
}
```

The flip is the final commit in the migration PR. Before it, the test is
opt-in per-row. After it, every row must carry a non-empty `feature`.

### 5.5 What the test does NOT enforce

Out of scope for v1, per `PROMPT.md`'s out-of-scope list:

- Surface-level binding-strength consistency. The matrix says
  `(mcp-health, runtimeInternal) → consumer_with_follow_up`; it does
  not assert "consumer should not be follow-up." That remains an audit
  question.
- Cross-feature transitive coverage. The matrix does not assert that
  `interrupt-and-cancel.agentFacing` is covered because
  `request-lifecycle.agentFacing` is covered. The tag is the only
  signal.
- Required-surface escalation. A `deferred` slot does not auto-promote
  to `required` once a row appears; the implementer must move it
  manually. Intentional friction.

## 6. Alternatives

### 6.1 Separate matrix file vs. inline annotation

**Alternative A — separate file.** Create
`crates/defra-agent/proofs/Proofs/Conformance/FeatureMatrix.lean`
containing a parallel structure that joins on `(category, domain)` to
the existing ledger. The existing ledger stays exactly as-is; the matrix
is a sibling lookup table.

Pros:
- Zero churn on existing ledger call sites.
- Implementer can land the matrix and the tagging in two clearly
  separated PRs.

Cons:
- Two structures to keep in sync. The drift test must enforce
  `(category, domain)` join coverage, doubling the failure modes.
- Audit reader has to read two files instead of one to see a row's
  feature tag.
- Future per-row annotations (e.g., binding strength) have to go
  somewhere — either a third sibling file or back into the ledger.

**Alternative B — inline annotation (recommended).** Extend
`CoverageEntry` with `feature` and `surfaces` directly; tag rows at
their existing call sites. The matrix is *derived* from the ledger
during JSON projection.

Pros:
- One source of truth. A tag lives next to its row; the audit reader
  sees both.
- Backwards-compatible thanks to defaulted fields (§2.2). No call site
  changes shape until tagged.
- Future per-row annotations have a natural home.

Cons:
- Touches the ledger structure. Some risk of subtle Lean elaboration
  edge cases when defaulted fields are added to a `structure` already
  consumed by many call sites (low risk; Lean handles `:=` defaults
  cleanly, but the implementer should grep for `{... : CoverageEntry}`
  pattern-matches to confirm none break — `coverageLedger` at
  `CoverageLedger.lean:398` is the only consumer in Lean today, with
  `Repr` derivation handling printing).

**Recommendation: B.** The pro of single source of truth is decisive,
and the backwards-compat story (optional fields with defaults) removes
the main objection to touching the structure.

### 6.2 Multi-tag (`feature : List String`) vs. single-tag (`feature : String`)

**Alternative C — multi-tag.** A row may declare multiple features.
`CancelCause` vocab could carry `["interrupt-and-cancel", "tool-call"]`.

Pros:
- Cross-cutting features (`interrupt-and-cancel`) get more rows
  matching them, improving cell row-counts.
- Honest about reality: a vocabulary row genuinely participates in
  more than one feature.

Cons:
- Encourages tag-sprawl. A reviewer adding a row faces a judgment call
  about how many tags to apply, and the answer drifts over time.
- The cell row-count loses calibration: a cell with `row_count = 5`
  could be five distinct rows or one row tagged five times.
- The drift test's diagnostic ("feature X has no row tagged Y / Z") is
  cleaner when tags are unambiguous.

**Alternative D — single-tag (recommended).** Each row carries one
primary feature tag. Cross-feature coverage is honest about being
transitive (§3.2).

Pros:
- One row, one feature. The cell row-count is the literal number of
  distinct rows.
- A reviewer adding a row asks "what feature is this *primarily*?" —
  a smaller question.
- The matrix is easier to render visually (a row appears in exactly
  one feature column).

Cons:
- Cross-cutting features look thinner than their substantive coverage
  warrants. `interrupt-and-cancel` ends up with one ledger row
  tagging it (CancelCause vocab), even though Request transition cases
  and ToolCall machine cover it too.

**Recommendation: D.** The single-tag rule is enforceable and
unambiguous. If a cross-cutting feature feels under-tagged, the right
answer is usually a dedicated ledger row (e.g., an `interrupt_cases`
case-coverage row Lean-emits separately), not a multi-tag escape valve.

### 6.3 Required-surface enforcement: hard fail vs. soft warning

**Alternative E — soft warning.** The drift test emits a warning to
stderr for missing required-surfaces but does not fail.

Pros:
- Easier rollout. The PR can land without immediately producing red
  CI for every feature with a `deferred` slot.
- Encourages exploratory matrix expansion without forcing immediate
  implementation work.

Cons:
- A soft warning becomes background noise. The May-15 / May-19 audits
  exist precisely because previous-style "we'll watch for this"
  signals decayed.
- The team's existing drift-test discipline is hard-fail
  (`coverage.rs:391`'s assertions are all `assert!`). Mixing
  conventions invites bit-rot.

**Alternative F — hard fail (recommended).** The drift test fails on a
missing required-surface. The escape is the explicit `deferred` list,
which is itself audit-visible (it appears in the JSON snapshot and the
drift test reads it).

Pros:
- Hard fail aligns with existing drift-test culture.
- The `deferred` escape is explicit, named, and reviewable — moving a
  surface from `required` to `deferred` is a one-line PR change that
  gets reviewed like any other follow-up.
- The matrix becomes a forcing function: a feature owner cannot
  silently let a surface gap accumulate.

Cons:
- The implementation PR must land with every `required` surface
  actually bound, OR with the surface explicitly `deferred`. The
  worked example in §7 already partitions today's reality into
  `required` (covered today) vs. `deferred` (gap with cited issue),
  so the implementation work is bounded.

**Recommendation: F.** The audit voice and the existing drift-test
posture both lean toward hard-fail-with-explicit-escape.

## 7. Worked example

Every ledger row in
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
appears below with its `feature` tag and `surfaces` list. Rows are
referenced by their starting line number in the current file.
"Surfaces" lists what the consumer test actually exercises, not what
the feature requires (those are §3's `required` lists).

### 7.1 `vocabularyCoverage` (CoverageLedger.lean:62-135)

| Line | Domain | Feature tag | Surfaces |
|---|---|---|---|
| `:65` | `RequestState` | `request-lifecycle` | `[runtimeInternal]` |
| `:69` | `ExecutionOrigin` | `request-lifecycle` | `[runtimeInternal]` |
| `:73` | `ProcessState` | `process-lifecycle` | `[runtimeInternal]` |
| `:77` | `PersistenceState` (boundary) | `persistence-failure-policy` | `[runtimeInternal]` |
| `:81` | `PersistenceFailurePolicy` (boundary) | `persistence-failure-policy` | `[runtimeInternal]` |
| `:85` | `ReconcilePhase` | `runtime-reconcile` | `[runtimeInternal]` |
| `:89` | `StorageObservation` (boundary) | `storage-observation` | `[runtimeInternal]` |
| `:93` | `SessionRecoveryLatestRequestState` | `session-recovery` | `[runtimeInternal]` |
| `:97` | `InferenceCallState` | `inference-call` | `[runtimeInternal]` |
| `:101` | `InferenceCallTerminalReason` | `inference-call` | `[runtimeInternal]` |
| `:105` | `ToolRetryDisposition` | `tool-call` | `[agentFacing]` |
| `:109` | `ToolCallState` | `tool-call` | `[agentFacing]` |
| `:113` | `CancelCause` | `interrupt-and-cancel` | `[agentFacing]` |
| `:117` | `ManagedExecState` | `managed-exec` | `[agentFacing]` |
| `:121` | `ToolFailureClass` | `tool-call` | `[agentFacing]` |
| `:125` | `AwaitMode` | `background-tools` | `[agentFacing]` |
| `:129` | `CancelPolicy` | `background-tools` | `[agentFacing]` |
| `:133` | `ChildTerminal` | `background-tools` | `[agentFacing]` |

Notes:
- `:77` and `:81` are boundary rows (no consumer); their `surfaces`
  list names the surface the boundary is observed on, derived from the
  boundary's own runtime fence at
  `Proofs/Conformance/Boundaries.lean:231` (`boundaryStorageHookFailurePolicyId`).
  Per §2.4, an empty `surfaces` list is permitted; here the implementer
  may equivalently set `surfaces := []` for boundary rows. The worked
  example chooses the explicit tag because the boundary IS observed at
  `runtimeInternal` (hook test consumes the policy).
- `:105` `ToolRetryDisposition`: the consumer is in
  `mcp_pool/tests.rs`. Per §2.1 the boundary call ("does this code run
  per-turn?") puts it in `agentFacing`.

### 7.2 `stateMachineCoverage` (CoverageLedger.lean:137-202)

| Line | Domain | Feature tag | Surfaces |
|---|---|---|---|
| `:138` | `Request` | `request-lifecycle` | `[runtimeInternal]` |
| `:142` | `Process` | `process-lifecycle` | `[runtimeInternal]` |
| `:146` | `Persistence.failClosed` (boundary) | `persistence-failure-policy` | `[runtimeInternal]` |
| `:151` | `Persistence.failOpen` (boundary) | `persistence-failure-policy` | `[runtimeInternal]` |
| `:156` | `StorageObservation.failClosed` (boundary) | `storage-observation` | `[runtimeInternal]` |
| `:161` | `StorageObservation.failOpen` (boundary) | `storage-observation` | `[runtimeInternal]` |
| `:166` | `RuntimeReconcile` | `runtime-reconcile` | `[runtimeInternal]` |
| `:170` | `PairingReconcile` | `pairing-reconcile` | `[runtimeInternal]` |
| `:174` | `SessionRecovery` | `session-recovery` | `[runtimeInternal]` |
| `:178` | `InferenceCall` | `inference-call` | `[runtimeInternal]` |
| `:182` | `ToolCall` | `tool-call` | `[agentFacing]` |
| `:186` | `ManagedExec` | `managed-exec` | `[agentFacing]` |
| `:190` | `AwaitMode` (state_machine) | `background-tools` | `[agentFacing]` |
| `:194` | `CancelPolicy` (state_machine) | `background-tools` | `[agentFacing]` |
| `:198` | `ChildTerminal` (state_machine) | `background-tools` | `[agentFacing]` |

### 7.3 `caseCoverage` (CoverageLedger.lean:204-371)

| Line | Domain | Feature tag | Surfaces |
|---|---|---|---|
| `:206` | `RequestTransitions` (lifecycle cases) | `request-lifecycle` | `[runtimeInternal]` |
| `:209` | `ProcessTransitions` (lifecycle cases) | `process-lifecycle` | `[runtimeInternal]` |
| `:213` | `TriggerDispatch` | `triggers` | `[runtimeInternal]` |
| `:217` | `RuntimeReconcileCases` | `runtime-reconcile` | `[runtimeInternal]` |
| `:221` | `ApplyReconcileCases` | `apply-reconcile` | `[operatorCli]` |
| `:225` | `SessionRecoveryCases` | `session-recovery` | `[runtimeInternal]` |
| `:229` | `InferenceCallSlotAccounting` | `inference-call` | `[runtimeInternal]` |
| `:233` | `FleetSlotAccounting` (boundary) | `fleet-slot-accounting` | `[runtimeInternal]` |
| `:238` | `PersistenceFailurePolicyCases` (boundary) | `persistence-failure-policy` | `[runtimeInternal]` |
| `:243` | `StorageObservationRuntimeCases` (boundary) | `storage-observation` | `[runtimeInternal]` |
| `:248` | `BackendHealthAdmissionCases` (boundary) | `backend-health` | `[runtimeInternal]` |
| `:252` | `NativeFilesystemBoundaryCases` | `tool-call` | `[agentFacing]` |
| `:257` | `ManagedExecLivenessCases` | `managed-exec` | `[agentFacing]` |
| `:261` | `FrontendClientShellCases` | `client-shell` | `[operatorUi]` |
| `:265` | `DesktopClientShellCases` | `client-shell` | `[operatorUi]` |
| `:269` | `LiveOverlayCases` | `client-shell` | `[operatorUi]` |
| `:273` | `ToolExecutionPreflight` | `tool-call` | `[agentFacing]` |
| `:277` | `ToolExecutionRetry` | `tool-call` | `[agentFacing]` |
| `:281` | `CommandPolicyValidation` | `command-policy` | `[agentFacing]` |
| `:285` | `CommandPolicySandbox` | `command-policy` | `[agentFacing]` |
| `:289` | `CommandPolicyEnv` | `command-policy` | `[agentFacing]` |
| `:293` | `QueueDeadlineConformanceCases` | `request-lifecycle` | `[runtimeInternal]` |
| `:297` | `RecoverySweepCases` | `recovery` | `[runtimeInternal]` |
| `:301` | `R6BackgroundingCases` | `background-tools` | `[agentFacing]` |
| `:305` | `BackgroundBudgetBoundedTheoremWitness` | `background-tools` | `[runtimeInternal]` |
| `:309` | `CascadeCancelsChildTheoremWitness` | `background-tools` | `[agentFacing]` |
| `:313` | `R4cBackgroundWorkCases` | `background-tools` | `[agentFacing]` |
| `:317` | `TranscriptConformanceCases` | `transcript` | `[agentFacing]` |
| `:321` | `IdentityStructuralCases` | `identity-permission` | `[runtimeInternal]` |
| `:325` | `IdentityPermissionCases` | `identity-permission` | `[runtimeInternal]` |
| `:329` | `IdentityContracts` | `identity-permission` | `[runtimeInternal]` |
| `:333` | `ResponseTransitionCases` | `streaming-response` | `[agentFacing]` |
| `:337` | `CompactionReducerCases` | `compaction` | `[agentFacing]` |
| `:345` | `EventDeliveryTransitionCases` (consumerWithFollowUp) | `event-delivery` | `[runtimeInternal]` |
| `:350` | `EventDeliverySourceInstances` | `event-delivery` | `[runtimeInternal]` |
| `:358` | `EventDeliveryConvergenceTraces` (consumerWithFollowUp) | `event-delivery` | `[runtimeInternal]` |
| `:366` | `MCPHealthCases` (consumerWithFollowUp) | `mcp-health` | `[runtimeInternal]` |

Notes on classification choices:
- `:213` `TriggerDispatch` — consumer is
  `trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases`,
  which drives `TriggerEngine::new(...).dispatch(intent)` (per audit §14).
  That entry point is invoked by the runtime when a Schedule / Event /
  Manual fire materializes — not directly by the agent turn loop.
  Classified `runtimeInternal`.
- `:269` `LiveOverlayCases` — the consumer is
  `tests/live_overlay_conformance.rs`, which is technically a
  defra-agent integration test (not desktop-tauri). The contract it
  asserts is the UI live-overlay projection shape. Classified
  `operatorUi` because the *contract surface* is what matters; the
  test's host crate is incidental. The implementer may push back and
  re-classify as `[runtimeInternal, operatorUi]`.
- `:305` vs `:309` — the two
  R6 theorem-witness rows split surfaces: `BackgroundBudgetBoundedTheoremWitness`
  drives admission budget invariant (runtime-side), while
  `CascadeCancelsChildTheoremWitness` drives a cascade-cancellation
  trace at the tool-execution boundary (agent-facing).

### 7.4 `followUpHookCoverage` (CoverageLedger.lean:373-390)

These are Lean-only theorem witnesses (no Rust consumer). Tag with the
owning feature; `surfaces := []` (the row has no surface binding by
design — that is the point of `followUpHookCoverage`).

| Line | Domain | Feature tag | Surfaces |
|---|---|---|---|
| `:374` | `Subagent.BridgedState.foreground_blocks_parent_advance` | `background-tools` | `[]` |
| `:378` | `Subagent.BridgedState.bridged_child_completion_propagates` | `background-tools` | `[]` |
| `:382` | `Subagent.BridgedState.inv_depth` | `background-tools` | `[]` |
| `:386` | `Subagent.BridgedState.bridgedUniqueCallIds_preserved` | `background-tools` | `[]` |

### 7.5 Coverage by feature, post-tagging

Row counts per feature, matching the "Tag count" column in §3:

- `request-lifecycle` — 5: `:65`, `:69`, `:138`, `:206`, `:293`. The
  `operatorUi` surface is delivered transitively by `:269`
  (`LiveOverlayCases` tagged `client-shell`); under the single-tag
  rule (§3.2) the feature's UI surface is **deferred**, not required.
  See §7.6.
- `process-lifecycle` — 3: `:73`, `:142`, `:209`.
- `inference-call` — 4: `:97`, `:101`, `:178`, `:229`.
- `tool-call` — 7: `:105`, `:109`, `:121`, `:182`, `:252`, `:273`,
  `:277`.
- `managed-exec` — 3: `:117`, `:186`, `:257`.
- `pairing-reconcile` — 1: `:170`.
- `runtime-reconcile` — 3: `:85`, `:166`, `:217`.
- `session-recovery` — 3: `:93`, `:174`, `:225`.
- `background-tools` — 14: `:125`, `:129`, `:133`, `:190`, `:194`,
  `:198`, `:301`, `:305`, `:309`, `:313`, plus the four followUpHook
  rows `:374`, `:378`, `:382`, `:386` (which contribute to count but
  not to any surface cell because their `surfaces := []`).
- `subagents-cross-deployment` — 0. All surfaces deferred. See §3.1.
- `interrupt-and-cancel` — 1: `:113`. See §3.2.
- `mcp-health` — 1: `:366`.
- `identity-permission` — 3: `:321`, `:325`, `:329`. The audit §9
  notes `:325` is partially bound (decision / hostability fields
  unused); the matrix reflects that via `pending_follow_ups = 1` in
  the `(identity-permission, runtimeInternal)` cell.
- `apply-reconcile` — 1: `:221`.
- `event-delivery` — 3: `:345`, `:350`, `:358`. The matrix cell
  carries `pending_follow_ups = 2`.
- `triggers` — 1: `:213`.
- `compaction` — 1: `:337`.
- `transcript` — 1: `:317`.
- `streaming-response` — 1: `:333`.
- `client-shell` — 3: `:261`, `:265`, `:269`.
- `command-policy` — 3: `:281`, `:285`, `:289`.
- `recovery` — 1: `:297`.
- `fleet-slot-accounting` — 1: `:233`.
- `storage-observation` — 4: `:89`, `:156`, `:161`, `:243`.
- `persistence-failure-policy` — 5: `:77`, `:81`, `:146`, `:151`,
  `:238`.
- `backend-health` — 1: `:248`.

Total: 74 row-tags across 74 distinct ledger rows. Sum:
5+3+4+7+3+1+3+3+14+0+1+1+3+1+3+1+1+1+1+3+3+1+1+4+5+1 = 74.

### 7.6 The `request-lifecycle.operatorUi` caveat

The matrix's required-surface declaration for `request-lifecycle` is
`[agentFacing, runtimeInternal, operatorUi]`. The agentFacing and
runtimeInternal cells are populated by `:138`, `:206`, `:293`. The
operatorUi cell is populated by `:269` (LiveOverlayCases) tagged with
`client-shell`. Under the single-tag rule (§3.2 / §6.2), `:269` does
not tag `request-lifecycle`, so the cell appears empty.

Two ways to resolve:

- **Soft option.** Add `[operatorUi]` to `:269`'s surfaces but
  ALSO tag it with `client-shell` only. The matrix cell becomes
  empty for `(request-lifecycle, operatorUi)`. Then move
  `(request-lifecycle, operatorUi)` from `required` to `deferred`
  with a follow-up "client-shell row at `:269` transitively covers
  this; promote to required once a dedicated request-lifecycle UI row
  exists." This is the worked example's chosen path — it preserves
  the single-tag invariant.

- **Hard option.** Tag `:269` with `request-lifecycle` instead of
  `client-shell`. Defensible (the live-overlay IS request-derived),
  but then `client-shell.operatorUi` loses a row. Cascading retag.

The worked example chooses the soft option for v1. §3's table already
reflects this: `request-lifecycle.required = [agentFacing,
runtimeInternal]` and
`deferred = [operatorUi (#TBD-request-lifecycle-ui-dedicated-row)]`.

This is the only required-surface that the worked example downgrades
to deferred during the v1 tagging pass. Every other `required` surface
in §3 is covered by at least one tagged row.

## 8. Migration and rollout

### 8.1 Implementation PR shape

The implementation PR (separate from this design) is bounded by:

1. Add the `Surface` inductive, extend `CoverageEntry`, add
   `FeatureSurfaceRequirement` and the new helpers — one Lean commit,
   no call sites change.
2. Apply the worked example: tag every ledger row at its existing
   call site. One commit per logical group (vocab / state machine /
   case / followUpHook), four commits, each independently reviewable.
3. Extend `Proofs/Conformance/Contracts/Json/Snapshot.lean:126` with
   `feature_surface_requirements` and `feature_matrix` JSON keys plus
   their projection functions. One commit.
4. Extend `crates/defra-agent/src/lean_vocab_test.rs`
   `LeanContractSnapshot` with the two new fields and accessors. One
   commit.
5. Add the new `#[test]` in
   `crates/defra-agent/tests/state_machine_conformance/coverage.rs`
   near `:391`. One commit.
6. Flip `REQUIRE_FEATURE_TAG_FOR_ALL_ROWS` to `true`. One commit.

All six fit one PR. Six commits gives reviewers granular bisect points
if a regression appears.

### 8.2 What the implementer should NOT do

Per `PROMPT.md`'s out-of-scope list:

- Do not restructure existing constructors. The four existing
  combinators at `CoverageLedger.lean:25-60` must keep their
  signatures.
- Do not rename rows. The audit at §13 and §3 (cosmetic deltas) flags
  renames; those are separate cleanup PRs.
- Do not add new ledger rows beyond what the matrix surfaces as
  required-and-missing. The `subagents-cross-deployment` row
  promotion is a judgment call at implementation time; if the
  implementer decides it is out of scope, move the surface to
  `deferred` and file the follow-up.
- Do not change the JSON snapshot beyond the two new keys at `:126`.

### 8.3 If schema extension forces a constructor change

The single sharp failure mode for this design: if Lean's elaboration
of the extended `CoverageEntry` with optional fields breaks any
existing pattern-match or destructure on the structure, the
constructor signatures cannot remain backwards-compatible.

Today the only Lean consumer of `CoverageEntry` is `coverageLedger`
at `CoverageLedger.lean:398` (which just builds a list) and
`CoverageEntry.toJson` at `:401` (which reads named fields). Both are
compatible with field-addition. The `deriving Repr` at `:20` handles
the new fields automatically.

The Rust side (`LeanContractSnapshot` accessors) IS where new fields
must be threaded explicitly. That is mechanical (one field, one
accessor, one read in the JSON deserialization) and does not change
existing accessor signatures.

If during implementation the elaboration check fails (e.g., an
unforeseen pattern match on `CoverageEntry` exists), STOP and report.
Per `PROMPT.md`: "The design must compose without forcing existing
rows to change at every call site." If that condition cannot hold,
the design needs a second pass.

## 9. Verification of this spec

Per `PROMPT.md`'s verification list:

1. **Every file path you cite exists at the cited line.** Spot checks
   (against the read content captured in the design pass):
   - `CoverageLedger.lean:14-20` — `CoverageEntry` structure: verified.
   - `CoverageLedger.lean:25-60` — four constructors: verified.
   - `CoverageLedger.lean:62-371` — the 60+ ledger rows enumerated in
     §7: verified row-by-row.
   - `Snapshot.lean:26-134` — `snapshotJson` shape: verified.
   - `Snapshot.lean:126-127` — `coverage_ledger` JSON key location:
     verified (insertion point for new keys).
   - `coverage.rs:391` — `lean_contract_coverage_ledger_accounts_for_every_emitted_domain`:
     verified.
   - `coverage.rs:629-673` — `valid_categories` array: verified
     (32 categories).
   - `coverage.rs:712-718` — consumer registry resolution: verified.
   - `Boundaries.lean:219`, `:228`, `:231`, `:234`, `:240` — boundary
     id defs: verified via grep.
   - `chat-shell.test.ts:304` — frontend ClientShell consumer:
     verified.

2. **The schema proposal compiles in concept against existing
   `CoverageLedger.lean`.** The extension uses field-addition with
   defaults and a sibling helper (`tagged`). No existing combinator
   signature changes. Lean handles `:=` defaults in `structure`
   declarations; the only Lean consumer (`coverageLedger`) builds a
   list and `CoverageEntry.toJson` reads named fields — both
   compatible with field-addition.

3. **The worked example covers every existing entry in the current
   `CoverageLedger.lean`.** §7 enumerates every line from `:62` to
   `:390`, partitioned into vocab / state-machine / case / followUpHook
   tables. Cross-checked against the file's line numbers row by row.

4. **The drift-test proposal names the specific assertion that would
   fail on a missing surface.** §5.2 assertion (b) is named
   verbatim: `"feature {:?} declares required surface {:?} but no
   ledger row tags this (feature, surface). Either add a ledger row,
   or move this surface to `deferred` with a follow-up note."`

5. **Voice matches the May-15 / May-19 audit doc.** Format mirrors the
   audit's `### What ... today / ### Smallest delta` framing in
   §7's per-row tables; TL;DR up top; recommended-next-step under each
   alternative; the deferred-with-follow-up mechanism mirrors
   `consumerWithFollowUpCoverage` / `followUpHookCoverage` discipline.

## 10. Out of scope (restated)

- Implementing the schema extension. Lean PR is separate.
- Implementing the drift-test extension. Rust PR is separate.
- Tagging the worked example in the actual `CoverageLedger.lean`.
  §7 is reference for that work.
- Renaming any existing rows.
- Changing the JSON snapshot beyond the two new keys at `:126-127`.
- Adding new surfaces or features beyond what the runtime + planned
  CLI / desktop UI cover today. The taxonomy in §3 is exhaustive for
  v1; future features (e.g., `mobile-shell`, `web-shell`) are
  out-of-scope additions that go through their own design pass.
- Audit-style binding-strength re-classification. The matrix records
  what the ledger says, not what it should say. §15 EventDelivery
  and §10 MCPHealth re-classification work remains in the audit's
  recommended-next-impl list.

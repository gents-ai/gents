# Lean-spec gap audit

Date: 2026-05-15

Branch: `design/lean-spec-gap-audit`

Scope:
- #185 / #193: `AgentPrincipal` / `AgentBehavior` / `AgentDeployment`
- ApplyReconcile, including #55 / #53 / #56 / #57 context
- Ledger follow-up rows that still look shape-pin or future-implementation oriented

Follow-ups filed from this audit:
- #219: Lean spec gap: executable identity permission cases (blocking #193)
- #220: Lean spec gap: production ApplyReconcile write-boundary cases (blocking #56)

Inline fixes from this audit:
- `597b01d Register subagent conformance ledger consumers`
- Draft PR: #218

## TL;DR

- ApplyReconcile is the stronger Lean-led implementation candidate today. Lean emits executable contract rows through the JSON snapshot, and `apply_conformance.rs` consumes those rows against the Rust reference `apply_model`; the remaining gap is production CLI write-boundary coverage, tracked in #220.
- #185 / #193 is not ready to drive the Rust refactor by a failing executable permission conformance test. Lean emits structural `World.WellFormed` cases and a deferred permission contract declaration, but no finite permission-decision contract rows and no Identity state machine.
- The Identity coverage ledger is accurate for the two emitted domains, but the `identity_contracts` consumer is a declaration check, not a runtime enforcement check. Treat it as a shape-pin until #219 adds executable cases and #193 consumes them.
- The stale ledger rows for AwaitMode, CancelPolicy, and ChildTerminal were mechanical gaps: Rust consumers already existed. They were promoted to `consumerCoverage` and registered in the consumer allowlist in `597b01d`.
- Recommended next-impl order: #219 first, then #193, then #220 plus #56, then the existing queue/recovery follow-up rows, then streaming/compaction runtime consumers.

## #185 / #193: Principal / Behavior / Deployment

### Issue state

#185 is closed. Its acceptance text says the Lean split should model agent principal, behavior, and deployment separately, emit conformance vectors, and have Rust consumer tests against permission invariants.

#193 is open. Its tracker says the current Rust runtime still conflates principal, behavior, and deployment in `DefraAgent`, and calls out the future permission module as the point where `identity.respects_principal_boundary` should flip from "contract present" to property-backed enforcement.

### What Lean models today

Lean has the trinity as separate records:

- `Principal` carries `did`, `displayName`, and `enabled` in `crates/defra-agent/proofs/Proofs/Identity/State.lean:17`.
- `Behavior` carries `id`, `principal`, `displayName`, and `enabled` in `crates/defra-agent/proofs/Proofs/Identity/State.lean:23`.
- `Deployment` carries `id`, `principal`, `hostId`, and `enabled` in `crates/defra-agent/proofs/Proofs/Identity/State.lean:30`.
- `World.WellFormed` requires unique principals, unique behaviors, unique deployments, behavior foreign keys, and deployment foreign keys in `crates/defra-agent/proofs/Proofs/Identity/State.lean:37`.

Lean also has permission-side predicates:

- `GrantStore` and `Decide` are declared in `crates/defra-agent/proofs/Proofs/Identity/Permission.lean:17`.
- `RespectsPrincipal` says decisions are equal for behaviors with the same principal in `crates/defra-agent/proofs/Proofs/Identity/Permission.lean:25`.
- `canonicalDecide` and `canonicalDecide_respects_principal` prove one abstract decision function satisfies that predicate in `crates/defra-agent/proofs/Proofs/Identity/Permission.lean:31`.

The proven properties include:

- Shared-principal permission sharing in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:15`.
- Different-decision isolation in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:25`.
- No escalation across principals in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:37`.
- Behavior-id-determines-principal in well-formed worlds in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:46`.
- Deployment hostability as `Deployment.canHostBehavior`, a Boolean principal equality check, in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:57`.

### What is emitted today

Identity emits two JSON domains:

- `identity_structural_cases`
- `identity_contracts`

The JSON snapshot appends them in `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean:729`.

`identity_structural_cases` is executable in the narrow structural sense. `IdentityStructuralCase` has principals, behaviors, deployments, and `expectedWellFormed` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:38`. The case list includes:

- `amy_general_and_amy_code_share_principal` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:46`.
- `amy_rumination_separate_principal` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:58`.
- `dangling_behavior_fk_violates` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:77`.
- `duplicate_behavior_id_violates` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:84`.
- `deployment_fk_violates` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:92`.
- `two_deployments_different_principals_ok` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:102`.

Those structural cases are serialized in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:144` and exposed as `structuralCasesJson` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:153`.

`identity_contracts` is not an executable permission-decision contract. It declares one deferred contract:

- `IdentityContract` has `name`, `statement`, `trackedBy`, and `enforced` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:157`.
- The emitted row is `identity.respects_principal_boundary`, points at `#193`, and sets `enforced := false` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:164`.

### Rust consumer state

The Rust snapshot schema includes `identity_structural_cases` and `identity_contracts` in `crates/defra-agent/src/lean_vocab_test.rs:67`, with the supporting Identity structs in `crates/defra-agent/src/lean_vocab_test.rs:603`.

The consumer test mirrors Lean's structural well-formedness rules:

- `rust_well_formed` implements the uniqueness and foreign-key checks in `crates/defra-agent/tests/identity_conformance.rs:18`.
- `identity_structural_cases_match_lean_verdicts` compares every Lean structural case against that Rust predicate in `crates/defra-agent/tests/identity_conformance.rs:53`.
- The named structural scenarios are pinned in `crates/defra-agent/tests/identity_conformance.rs:71`.

The permission-side test is explicitly a declaration check:

- `identity_respects_principal_contract_is_declared` reads the emitted contract in `crates/defra-agent/tests/identity_conformance.rs:91`.
- It asserts `enforced` is false and that `tracked_by` is `#193` in `crates/defra-agent/tests/identity_conformance.rs:107`.
- The test comment says #193 should replace the assertion with a property-based runtime `decide` test when the permission module lands in `crates/defra-agent/tests/identity_conformance.rs:102`.

The production Rust shape is still pre-refactor:

- `DefraAgent` keeps `agent_did`, `default_behavior_id`, `behaviors`, and `unavailable_behaviors` together in `crates/defra-agent/src/agent.rs:86`.
- Agent loading still derives a default behavior id from resolved documents in `crates/defra-agent/src/agent.rs:111`.
- `behavior_config_from_documents` takes identity, behavior, backend, profile, and tools together and builds `BehaviorConfig` in `crates/defra-agent/src/agent.rs:201`.

The design note also says this was intentional staging: the runtime still conflates the concepts, no `AgentDeployment` schema/struct exists, and no permission engine exists yet in `docs/superpowers/specs/2026-05-13-identity-split-lean-design.md:15`.

### Are executable contract cases present?

Only for structural well-formedness.

For #193's permission/refactor boundary, the current Lean output is shape-pin only:

- There is no finite row saying "given behavior A, behavior B, permission P, grant store G, Rust decision must be X/Y."
- There is no finite row tying deployment hostability to runtime deployment placement.
- There is no emitted witness for the future permission module satisfying `Identity.RespectsPrincipal decide`.

The structural rows can catch Rust mistakes in uniqueness and foreign-key handling, but they do not fence the permission decision behavior that #193 names as the Rust refactor contract.

### Are decidable transitions present?

Not as an Identity machine.

There are decidable ingredients:

- `Deployment.canHostBehavior` is executable Boolean principal equality in `crates/defra-agent/proofs/Proofs/Identity/Properties.lean:57`.
- `canonicalDecide` is an executable abstract permission decision in `crates/defra-agent/proofs/Proofs/Identity/Permission.lean:31`.
- `World.WellFormed` is evaluated into JSON structural cases through `expectedWellFormed` in `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:38`.

But there is no `Identity` state machine in `Proofs/Conformance/Contracts/Machines.lean`. The `stateMachines` list includes request, process, persistence, storage observation, runtime reconcile, pairing reconcile, session recovery, inference call, tool call, and subagent vocabulary machines, but not Identity, in `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines.lean:564`.

For dispatch, this means #193 does not yet have a red Rust conformance test for permission behavior. It has a green shape/structure consumer and a green "contract is declared but deferred" consumer.

### Coverage ledger accuracy

The ledger rows are accurate for the domains that exist:

- `identity_structural_cases` is `consumerCoverage` with Rust consumer `identity_conformance::identity_structural_cases_match_lean_verdicts` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:299`.
- `identity_contracts` is `consumerCoverage` with Rust consumer `identity_conformance::identity_respects_principal_contract_is_declared` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:303`.

The important caveat is semantic: `identity_contracts` is consumer-covered as a declaration row, not as enforced runtime behavior. The name of the Rust consumer is accurate; the domain should not be read as "permission conformance exists."

I did not change this ledger row inline because the emitted artifact is in fact consumed. The larger problem is that the emitted artifact is the wrong strength for #193, so #219 tracks the missing executable Lean rows.

### Smallest Lean delta to unblock #193

Issue #219 should be the next Lean-first task before the Rust refactor is dispatched.

Minimum useful addition:

1. Add finite executable permission cases under `Proofs/Identity/Conformance.lean`.
   - Include at least behavior A, behavior B, permission, grant store, expected decision for A, expected decision for B, and an explicit same-principal/different-principal expectation.
   - Include a hostability case for behavior/deployment principal equality if #193 will introduce `AgentDeployment` placement logic.

2. Emit those rows in `Proofs/Conformance/Contracts/Json.lean`.
   - A new snapshot key such as `identity_permission_cases` is enough.
   - The rows should be executable and deterministic, not only text statements.

3. Add the coverage ledger row as `consumerWithFollowUpCoverage` or `followUpCoverage` until the Rust consumer lands.
   - Once #193 consumes it, promote it to `consumerCoverage`.

4. In #193, consume the same rows from Rust.
   - Replace `identity_respects_principal_contract_is_declared` with a runtime permission-decision consumer.
   - Flip `identity.respects_principal_boundary.enforced` from false to true only after Rust satisfies the cases.

This is small enough to be a focused Lean PR, but it is not a mechanical inline fix because it defines the contract surface for the Rust refactor.

## ApplyReconcile

### Issue state

#55 is closed, but its body points at the broader apply/reconcile cleanup that ultimately landed through #53.

#53 is closed and matches the current code shape: `Collection`, typed desired/live field markers, and apply conformance/property tests exist.

#56 is open for transactional `defra-agent-cli config apply`.

#57 is open for delete semantics when live-only removal is introduced.

### What Lean models today

Lean has a complete executable reference model for the current create/update/no-delete apply semantics:

- `Collection` variants are declared in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Collections.lean:17`.
- `Collection.applyOrder` ranks foundational collections before behavior, behavior before task/schedule, and principal/event-trigger last in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Collections.lean:31`.
- `DesiredFields`, `LiveFields`, `Manifest`, and `LiveState` are declared in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Manifest.lean:17`.
- `Manifest.WellFormed` enforces reference closure and strictly lower apply-rank references in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Manifest.lean:66`.
- `ApplyStep` has only `create` and `update` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Diff.lean:15`.
- `diff` emits creates and updates, treats live-only documents as no-op, and sorts by apply order in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Diff.lean:51`.
- `applyOne` and `applyAll` update desired state only in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Apply.lean:11`.
- `apply_preserves_live` proves apply does not mutate live state in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ApplyProperties.lean:17`.
- `LiveState.toResolvedSnapshot` is the runtime bridge witness in `crates/defra-agent/proofs/Proofs/ApplyReconcile/RuntimeBridge.lean:61`.

The convergence facts are also modeled:

- `t_conv_runnable` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Convergence.lean:16`.
- `t_conv` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Convergence.lean:70`.
- `t_conv_no_unavailable` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Convergence.lean:80`.
- `t_conv_published` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/Convergence.lean:106`.

### What is emitted today

ApplyReconcile emits executable contract cases.

`ContractCases.lean` states the intent directly: these finite witnesses are emitted through `Proofs.Conformance.Contracts` and are executable conformance cases, not a Rust-only table, in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:7`.

The case model includes:

- `ContractDoc`, `ContractLiveDoc`, `ContractStep`, `ApplyReconcileScenario`, and `ApplyReconcileCase` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:23`.
- `diffSteps`, which computes expected steps from desired/live input, in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:132`.
- `buildCase`, which computes expected buckets, steps, prefix behavior, retry behavior, idempotence, and reference closure flags, in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:203`.

The emitted scenario set includes:

- `empty_manifest` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:260`.
- `backend_before_behavior_ordering` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:267`.
- `update_existing_backend` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:276`.
- `live_only_no_op` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:282`.
- `prefix_retry_convergence_idempotence` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:288`.
- `referrer_closure` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:298`.

The cases are serialized by `applyReconcileCasesJson` in `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases.lean:348`.

The JSON snapshot includes `apply_reconcile_cases` in `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean:659`.

### Coverage ledger accuracy

The ledger currently says:

- `apply_reconcile_cases` has `consumerCoverage` with Rust consumer `apply_conformance::generated_apply_reconcile_cases_drive_apply_model_and_production_ordering` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:209`.

That is accurate for the current emitted domain. The Rust test consumes every Lean ApplyReconcile case and checks both the Rust reference model and production ordering constants.

The caveat is that the production CLI write boundary is not fully Lean-driven yet. The ledger row does not mean `defra-agent-cli config apply` is transactionally fenced by Lean rows. It means the Lean cases drive `defra_agent::apply_model`, and the same test cross-checks the production collection ordering constant.

I filed #220 because the next operational boundary for #56 needs its own Lean-backed production consumer.

### Rust consumer state

The snapshot schema includes `apply_reconcile_cases` in `crates/defra-agent/src/lean_vocab_test.rs:40`, with the ApplyReconcile case structs in `crates/defra-agent/src/lean_vocab_test.rs:260`.

`apply_conformance.rs` is wired to the Lean rows:

- It imports the Rust reference model from `defra_agent::apply_model` in `crates/defra-agent/tests/apply_conformance.rs:9`.
- It imports Lean Apply case accessors and structs in `crates/defra-agent/tests/apply_conformance.rs:19`.
- It maps Lean collection names to Rust `Collection` in `crates/defra-agent/tests/apply_conformance.rs:54`.
- It converts Lean manifests and live states into Rust `apply_model::Manifest` and `apply_model::LiveState` in `crates/defra-agent/tests/apply_conformance.rs:85`.
- It compares Rust `ApplyStep`s to Lean expected steps in `crates/defra-agent/tests/apply_conformance.rs:166`.
- The main test consumes all generated Lean cases, checks required scenario names, buckets, steps, production apply order, lower-rank references, prefix behavior, complete apply, manifest realization, retry convergence, and idempotence in `crates/defra-agent/tests/apply_conformance.rs:207`.

The `apply_model` itself is explicitly a reference implementation:

- The module comment says it mirrors Lean and that production apply lives in `defra-agent-cli` in `crates/defra-agent/src/apply_model.rs:1`.
- Its `diff` logic starts in `crates/defra-agent/src/apply_model.rs:108` and sorts by collection apply order plus document id in `crates/defra-agent/src/apply_model.rs:135`.
- Its `apply_one` and `apply_all` preserve live state in `crates/defra-agent/src/apply_model.rs:146`.

`apply_property.rs` is useful, but it is not Lean-led:

- The property tests synthesize Rust manifests and live states independently in `crates/defra-agent/tests/apply_property.rs:161`.
- They validate the same model-family properties, but they are parallel property coverage rather than generated Lean case consumption.

The production CLI surface is separate:

- `CONFIG_APPLY_ORDER` lives in `crates/defra-agent-cli/src/config_import.rs:24`.
- `apply_desired_state_changes` iterates that order and applies selected documents in `crates/defra-agent-cli/src/config_import.rs:591`.
- `select_apply_docs_for_collection` selects the create/update documents for one collection in `crates/defra-agent-cli/src/config_import.rs:615`.
- Existing CLI tests pin order and retry-safe prefixes in `crates/defra-agent-cli/src/config_import.rs:642` and `crates/defra-agent-cli/tests/cli_config_apply_order.rs:4`.

### Is this a Rust-catches-up-to-Lean candidate?

Yes for the reference model and collection ordering.

Not yet for the production write boundary.

The current Lean cases are strong enough to catch regressions in:

- collection vocabulary parity,
- apply order,
- desired/live bucket classification,
- create/update step computation,
- live-only no-op behavior,
- prefix retry behavior,
- idempotence,
- lower-rank reference closure.

They are not yet enough to catch bugs in:

- actual `config apply` write sequencing,
- production document selection per collection,
- transactional rollback behavior for #56,
- future delete behavior for #57.

### Smallest additions to make ApplyReconcile production-ready

Issue #220 is the next narrow addition.

Minimum useful addition:

1. Either reuse existing `apply_reconcile_cases` directly in `defra-agent-cli`, or add production-facing expected fields only if the existing rows are too model-shaped.

2. Add a `defra-agent-cli` conformance test that consumes Lean rows and checks:
   - `CONFIG_APPLY_ORDER` matches Lean `Collection.applyOrder`;
   - selected create/update documents match expected Lean buckets;
   - no live-only document is selected for write;
   - prefixes are retry-safe and lower-rank references appear before referrers.

3. Keep delete behavior out of this task.
   - Lean and Rust currently model create/update/no-op only.
   - #57 should own live-only deletion semantics and any new Lean rows for delete.

4. Use #56 for the transactional implementation once the production boundary is fenced.

This is smaller than a Lean refactor because the existing rows already contain most of the needed data. The main gap is consumer placement: `defra-agent-cli`, not `defra_agent::apply_model`.

## Other shape-pin or follow-up ledger entries

The following rows still deserve dispatch attention because their ledger status or follow-up text points at implementation work beyond the current Rust consumer.

### Queue deadline cases

`queue_deadline_cases` is `consumerWithFollowUpCoverage` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:277`.

The Rust consumer pins generated queue/deadline contract rows in `crates/defra-agent/tests/state_machine_conformance.rs:3291`, but the ledger follow-up says runtime-backed queue/deadline consumers land in R4a Task 5 and Task 7. This looks like a good future implementation target once the current Identity and ApplyReconcile dispatches are clear.

### Recovery sweep cases

`recovery_sweep_cases` is `consumerWithFollowUpCoverage` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:282`.

The Rust tests check generated recovery sweep basics and obligations in `crates/defra-agent/tests/state_machine_conformance.rs:787`, including explicit obligation coverage for detached bridge cleanup and `InferenceCall::recover_all` in `crates/defra-agent/tests/state_machine_conformance.rs:853`.

This row is more actionable than a pure shape-pin because the tests already name concrete missing runtime work.

### Streaming response cases

`streaming_response_cases` is `consumerWithFollowUpCoverage` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:307`.

The consumer pins the generated lifecycle contract in `crates/defra-agent/tests/state_machine_conformance.rs:1745`. The ledger follow-up says runtime-backed streaming response lifecycle drive remains future work.

### Compaction reducer cases

`compaction_reducer_cases` is `consumerWithFollowUpCoverage` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:312`.

The consumer pins the generated compaction reducer contract in `crates/defra-agent/tests/state_machine_conformance.rs:2037`. The ledger follow-up says runtime-backed reducer drive remains future work.

### Subagent vocabulary rows fixed inline

AwaitMode, CancelPolicy, and ChildTerminal no longer belong in this follow-up bucket.

The ledger now records Rust consumers for those rows:

- `await_mode_vocab` uses `state_machine_conformance::lean_emits_await_mode_vocabulary` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:115`.
- `cancel_policy_vocab` uses `state_machine_conformance::lean_emits_cancel_policy_vocabulary` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:119`.
- `child_terminal_vocab` uses `state_machine_conformance::lean_emits_child_terminal_vocabulary_and_projections` in `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:123`.

The same consumers are registered in the coverage allowlist in `crates/defra-agent/tests/support/conformance_consumers.rs`.

## Recommended next-impl order

1. #219: Identity executable permission cases.

   Reason: #193 is the named Rust refactor candidate, but the Lean side currently emits only structure plus a deferred contract declaration. A small Lean addition can turn #193 from "refactor against shape" into "make this red conformance test green."

2. #193: Rust Principal / Behavior / Deployment refactor and permission consumer.

   Reason: once #219 lands, #193 can consume finite permission and hostability rows while splitting the runtime shape currently concentrated in `DefraAgent`. This has high architectural impact, but it should not start as a broad Rust-only reshuffle.

3. #220, then #56: ApplyReconcile production write-boundary conformance and transactional apply.

   Reason: ApplyReconcile is already Lean-ready for the reference model. The next useful work is moving the Lean-backed check closer to `defra-agent-cli config apply`, then using it to guide transactional semantics for #56. This has direct operational impact and low Lean uncertainty.

4. Recovery sweep obligations.

   Reason: `recovery_sweep_cases` already names concrete runtime gaps, including detached bridge cleanup and `InferenceCall::recover_all`. That makes it easier to dispatch than broader lifecycle areas.

5. Queue/deadline runtime-backed consumers.

   Reason: the generated rows exist and are consumed as contract pins, but the ledger still points at runtime-backed claim/deadline implementation tasks.

6. Streaming response and compaction reducer runtime drive.

   Reason: both have generated contract rows and consumers, but the ledger still treats runtime-backed lifecycle/reducer execution as follow-up. Pick these when the related runtime surfaces are ready to move, not before Identity and ApplyReconcile are unblocked.

Dispatch summary:

- Best immediate Lean-first task: #219.
- Best immediate Rust-after-Lean task: #193.
- Best already-ready implementation stream: ApplyReconcile reference semantics, with #220 needed to make production CLI apply Lean-led before #56.
- Avoid starting delete semantics in this pass; #57 should own that model and contract expansion.

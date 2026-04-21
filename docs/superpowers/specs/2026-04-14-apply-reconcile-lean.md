# Apply-Reconcile Convergence: Lean Specification and Rust Groundwork

**Date:** 2026-04-14
**Status:** Draft — brainstormed, awaiting user review
**Scope:** Close the formal loop between the CLI apply path and the runtime reconcile path by proving end-to-end convergence; introduce Rust groundwork that makes the CLI apply surface correspond 1:1 with the Lean model.

## Summary

The existing Lean proofs cover the runtime half of reconciliation — `RuntimeReconcile.lean` models everything from the moment a document write lands in DefraDB through the publication of an `ActiveRuntimeSnapshot` that the router and scheduler observe. The operator/CLI half — manifest loading, validation, diffing against live state, and the ordered sequence of writes that constitutes an apply — has grown substantially in recent commits and is currently unmodeled.

This spec proposes a single additional theorem, **T-Conv (end-to-end convergence)**, that states: starting from any well-formed manifest `M` and any live state `L`, applying `diff M L` and letting `RuntimeReconcile` run to quiescence yields a published runtime snapshot whose behavior set equals `M`'s. Everything else that might have been formalized around the apply path is explicitly redirected elsewhere — to Rust types, to property tests, or to deferred work — because formalizing it in Lean would be ceremony without unique payoff.

Alongside the proof, this spec scopes the Rust work needed to make the CLI apply surface correspond cleanly to the model: a first-class `Collection` enum, a typed `DesiredFields` / `LiveFields` partition at the apply boundary, `proptest`-based property tests for diff and apply ordering, and a small table-driven conformance test. It also calls out the apply-atomicity gap as an explicit known limitation and documents the follow-on work to file as issues.

## Why This Scope

The apply path is a state machine in the loose sense, but most of what formal methods would buy us on it is already better addressed by other tools:

- **Non-interference between apply and runtime** (apply writes only desired-state fields; runtime writes only live-state fields) is a type-system property once we split the field-owner partition at the Rust API boundary. Lean adds nothing a well-typed `DesiredFields` / `LiveFields` split does not.
- **Referential-integrity ordering** (apply writes parents before children) is a topological sort. Property tests with generated manifests catch ordering regressions with tighter feedback than a Lean proof, and they generalize as the schema evolves without re-editing theorems.
- **End-to-end convergence** — "after apply plus reconcile, the runtime's published snapshot reflects the manifest" — is the only statement that spans two subsystems and is not provable from either model alone. It is also the statement operators implicitly rely on when they edit a manifest, run `apply`, and expect the runtime to reflect their change. This is where Lean earns its keep on this path.
- **Round-trip and idempotence** (`export ∘ apply ∘ export ≡ export ∘ apply`; `apply ∘ apply ≡ apply`) are classic property-test targets; formalizing them in Lean is over-engineering unless a regression shows up.
- **Apply atomicity** (what happens on partial failure mid-apply) is a real operator-UX concern but is not a state-machine property; the current behavior is best-effort with no rollback, and T-Conv explicitly assumes apply runs to completion.

The result is a smaller formal surface (one theorem, one new module) than an earlier framing with multiple theorems and is honest about what Lean uniquely protects against on this path.

## Model

A new module `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean` imports `Proofs.Basic`, `Proofs.RuntimeReconcile`, and `Mathlib.Data.Finset.Basic`. It defines:

- `Collection` — an inductive with one constructor per collection under operator control: `AgentPrincipal`, `AgentBehavior`, `ToolSelection`, `InferenceBackend`, `InferenceProfile`, `ToolServiceRegistry`, `ScheduledTask`. This enum is the Lean counterpart of the Rust `Collection` enum (see Rust groundwork).
- `DocRef` — a pair of `Collection` and document id, with `DecidableEq`.
- `DesiredFields` and `LiveFields` — per-collection projections capturing which fields the apply agent owns versus which the runtime owns. The model does not enumerate every field; it abstracts over them as opaque typed projections so proofs do not need to be re-edited when a single field is added.
- `Manifest` — a finite partial map from `DocRef` to `DesiredFields`.
- `LiveState` — a finite partial map from `DocRef` to `(DesiredFields × LiveFields)`.
- `References : DesiredFields → Finset DocRef` — captures cross-document references (behavior→backend, behavior→tool-selection, behavior→profile, task→behavior).
- `WellFormed (M : Manifest)` — every reference target is in `M.docs`.
- `ApplyStep` — `create (d : DocRef) (f : DesiredFields) | update (d : DocRef) (f : DesiredFields)`. Intentionally no `delete` constructor, matching current CLI semantics (live-only documents are reported but not removed).
- `diff : Manifest → LiveState → List ApplyStep` — pure function producing the ordered step list. The four-bucket accounting (`create`, `update`, `unchanged`, `live_only`) used by the CLI for reporting is a separate diagnostic view; only `create` and `update` produce steps.
- `applyOne : LiveState → ApplyStep → LiveState` — models a single write landing in the DB. Composed under fold, this is the apply side of the write boundary that `RuntimeReconcile` consumes.

### Theorem

**T-Conv (end-to-end convergence).** For any `WellFormed` manifest `M` and any `LiveState L` satisfying a consistency precondition (its desired-field projection is itself well-formed under its existing references), the following holds:

Let `L' = fold applyOne L (diff M L)`. Compose `L'` with `RuntimeReconcile` running to quiescence. The resulting `ActiveRuntimeSnapshot.runnable ∪ unavailable` equals the set of behavior ids in `M`.

Convergence relies on `RuntimeReconcile`'s existing liveness properties for the generation-publishing side; this proof plugs the apply half into that model and closes the composition.

### Non-goals

- No proof of non-interference at the Lean level; the Rust type-system split carries that property structurally.
- No proof of ordering-preservation under apply; property tests cover it.
- No proof of delete safety; the model has no `delete` constructor. When live-only removal lands, a separate theorem T-Delete-safety will be added.
- No proof of round-trip or idempotence.

## Rust Groundwork

Two pieces of Rust work are in scope as prerequisites for making the conformance correspondence honest rather than implicit.

### Collection enum

A new `enum Collection` in `defra-agent-cli` replaces the parallel `*_FILE` string constants and the seven-way dispatch branches in `desired_state.rs`. Variants mirror the Lean inductive exactly. This enum becomes the single place that encodes the set of operator-controlled collections, and it gives every downstream routine (file naming, diff accounting, error reporting, apply dispatch) a typed discriminator.

### DesiredFields / LiveFields partition

The API boundary between apply-side writes and runtime-side writes surfaces a typed split: apply code paths produce values of a type that can only represent desired-state fields, and runtime code paths produce values of a type that can only represent live-state fields. The exact shape — per-collection trait vs. single enum with per-variant struct vs. macro-generated families — is an implementation-plan decision, not a spec decision. What the spec requires is that the violation of non-interference (apply writing a live-state field, or vice versa) be unrepresentable, not merely prevented by discipline.

## Testing

### Property tests

A new file `crates/defra-agent/tests/apply_property.rs` with `proptest` generators for `Manifest`. Properties:

1. For any `M, L`, `diff M L`'s four buckets (`create`, `update`, `unchanged`, `live_only`) partition the union of `M`'s and `L`'s document ids with no overlap.
2. For any `WellFormed M`, applying `diff M L` in the declared collection order yields an intermediate `LiveState` after every step whose active references are all to documents already present.
3. `diff` is deterministic: for two `Manifest` values that are equal as maps, `diff` produces step lists equal up to the declared collection order regardless of underlying iteration order.

### Conformance tests

A small new file `crates/defra-agent/tests/apply_conformance.rs` with table-driven cases pinning `(manifest, initial_live_state) → expected_apply_steps → expected_final_live_state`. Style mirrors `state_machine_conformance.rs`. Intentionally minimal — the proof and property tests carry the correctness load; conformance anchors the Lean model to concrete Rust output on a handful of representative inputs.

### Test cleanup

`crates/defra-agent-cli/tests/cli_e2e.rs` currently contains roughly forty-four integration tests covering CLI invocations end to end. Some of these tests assert apply or diff correctness by running the CLI against a live node and inspecting DB state; those overlap with what the property tests and conformance tests now cover directly. During implementation, the work includes an audit pass:

- Tests whose assertion is a correctness claim about diff buckets, apply ordering, or post-apply state are candidates for removal once the equivalent property test exists.
- Tests that cover CLI-surface concerns (exit codes, stdout/stderr formatting, error messages, file I/O behavior, manifest file layout, auth) are kept — they are not subsumed.

The audit is part of the implementation plan, not the spec: the spec's commitment is that the new tests will exist, and that the overlap will be resolved rather than left as redundant coverage.

## Known Limitations

**Apply atomicity.** Today, if `defra-agent-cli apply` fails partway through writing the step list (DB error, schema violation discovered late, crash), the database is left in a partially-updated state. There is no rollback. T-Conv assumes apply runs to completion; it does not cover crash-mid-apply.

This is called out explicitly here so that no one later reads T-Conv as a guarantee that it is not. Making apply transactional is the natural follow-on operator-ergonomics work and is filed as a separate issue.

## Follow-on Issues

- **I-1 (#55, from code review):** Consolidate desired-state collection handling in `desired_state.rs`. The seven near-identical `Desired*` structs, the seven near-identical diff fields, and the seven parallel apply branches could share a trait, a macro, or a shared enum. Not blocking; flagged for when a second motivating refactor appears.
- **I-2 (#56, from apply-atomicity discussion):** Make apply transactional — on partial failure, roll back to the pre-apply state. This is the natural next-step UX work once T-Conv lands.
- **I-3 (#57, optional, tracking):** Model delete semantics when live-only removal is added. Stub for the future T-Delete-safety theorem.

## Out of Scope

- Export/import round-trip proofs or property tests beyond the diff-level determinism listed above.
- Refactoring `main.rs` structure (currently 6534 lines) beyond what introducing the `Collection` enum forces.
- The `Desired*` struct repetition refactor (I-1).
- Apply atomicity implementation (I-2).
- Delete semantics (I-3).

## Deliverables Checklist

- [x] `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean` — new module with `Collection`, `DocRef`, `Manifest`, `LiveState`, `ApplyStep`, `diff`, `applyOne`, and T-Conv.
- [x] `crates/defra-agent/proofs/Proofs.lean` — register the new module.
- [x] `defra-agent-cli` — introduce `enum Collection` and thread it through `desired_state.rs`, `main.rs` dispatch, file naming.
- [x] `defra-agent-cli` (or shared crate) — typed `DesiredFields` / `LiveFields` at the apply-write boundary.
- [x] `crates/defra-agent/tests/apply_property.rs` — `proptest` properties listed above.
- [x] `crates/defra-agent/tests/apply_conformance.rs` — table-driven conformance cases.
- [x] `crates/defra-agent-cli/tests/cli_e2e.rs` — audit pass to remove tests subsumed by the new coverage.
- [x] Apply-atomicity known-limitation note in `crates/defra-agent/proofs/README.md`.
- [x] Issues I-1, I-2, I-3 filed.

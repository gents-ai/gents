# Apply-Reconcile — Finish the Promised Work

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Close the gap between what PR #59 landed and what the spec at `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` promised. The critical review on 2026-04-21 identified that:

- T-Conv as landed says `desired d = some f`, not the spec's "`ActiveRuntimeSnapshot.runnable ∪ unavailable = behavior_ids(M)`".
- `apply_preserves_wellFormed` is vacuous because `referencesOf = ∅`.
- `apply_property.rs::apply_ordering_preserves_references` is vacuous for the same reason.
- Production `apply_desired_state_changes` writes `AgentPrincipal` LAST; `Collection::apply_order()` says it's rank 1.
- `apply_model` is parallel simulation, not anchored to production.
- `Collection` is defined three times (CLI, apply_model, Lean) with no drift guard.
- `DesiredApplyBundle::from_trusted_bundle` is `pub(crate)`, not truly private.

**Architecture:** Three phases. Phase A fixes the Rust-side honesty issues (production order, consolidation, visibility). Phase B substantiates the abstract `referencesOf` so the "ordering preserves references" property has teeth. Phase C rebuilds T-Conv to actually compose through `RuntimeReconcile`.

**Tech Stack:** Rust (defra-agent workspace), Lean 4 + Mathlib.

---

## Phase A — Rust honesty fixes

### Task A1: Fix production apply order + anchor test

**Files:**
- Modify: `crates/defra-agent-cli/src/config_import.rs`
- Create: `crates/defra-agent-cli/tests/cli_config_apply_order.rs`

**Required order** (matching `Collection::apply_order` ranks):

1. InferenceBackend, InferenceProfile, ToolServiceRegistry, ToolSelection (rank 0 — leaves)
2. AgentPrincipal (rank 1)
3. AgentBehavior (rank 2)
4. ScheduledTask (rank 3)

Current production writes principal LAST. Move it to position 5 (between rank-0 writes and AgentBehavior).

Write a unit-level test `cli_config_apply_order_matches_collection_apply_order` that constructs a `DesiredStateManifest` with all 7 collection types (including a principal + a behavior), captures the order of `apply_import_collection` calls via a recording `ConfigAccess` mock, and asserts the sequence matches `Collection::ALL.sorted_by_key(|c| (c.apply_order(), c.graphql_type()))`.

### Task A2: Move `Collection` to the library crate

**Files:**
- Create: `crates/defra-agent/src/collection.rs` (new home)
- Delete: `crates/defra-agent-cli/src/collection.rs` (old home)
- Modify: `crates/defra-agent/src/lib.rs` (register `pub mod collection;`, re-export `Collection`)
- Modify: `crates/defra-agent-cli/src/main.rs` (delete `mod collection;`)
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs` (use `defra_agent::Collection`)
- Modify: `crates/defra-agent-cli/src/config_import.rs` (same)
- Modify: `crates/defra-agent/src/apply_model.rs` — delete its local `Collection` enum, use `crate::Collection` instead. Adjust tests.

The single source of truth is now `defra_agent::Collection`. The `apply_model` reference impl uses it. The CLI uses it. The Lean inductive still needs a counterpart (kept separate; drift-guard added via parity test).

### Task A3: Make `from_trusted_bundle` truly private to `desired_state`

**Files:**
- Move `DesiredApplyBundle` from `shared.rs` into `desired_state/mod.rs` (or `desired_state/apply_bundle.rs`).
- Change visibility: `inner` stays private; `from_trusted_bundle` becomes `pub(super)` (only `desired_state::convert` can call it). `as_bundle` stays `pub(crate)`.
- Update imports in `config_import.rs` and `commands/config/apply.rs`.

### Task A4: Cross-language Collection parity test

**Files:**
- Create: a new test case in `crates/defra-agent/tests/apply_conformance.rs` (or a new `tests/collection_parity.rs`).

Rust-level test that asserts `Collection::ALL` produces the same sequence of `(graphql_type, apply_order)` tuples as a hand-coded canonical list. Then a comment pointing at the Lean inductive with the same list, and a reminder that adding a variant requires updating both.

Also: add a Lean-side `#check` / `example` at the bottom of `ApplyReconcile.lean` that pattern-matches on all 7 `Collection` cases to force exhaustivity. If the Lean inductive ever drifts, the Lean build fails.

---

## Phase B — Substantiate `referencesOf`

### Task B1: Change `DesiredFields` in Lean to carry reference edges

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

Replace `abbrev DesiredFields := String` with:

```lean
structure DesiredFields where
  content : String
  refs    : Finset DocRef
  deriving Repr
```

Update `referencesOf : DesiredFields → Finset DocRef := fun f => f.refs`.

The `DesiredFields` binder in `ApplyStep.create`/`update`, `applyOne`, `applyAll`, `diff` threads through unchanged. Check that `apply_realizes_manifest` still closes — its proof doesn't use `DesiredFields` structure, only equality.

### Task B2: Make `diff`'s ordering explicit

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

Add decidable LT on `Collection` (inferable from `DecidableEq` + a lookup into `applyOrder`), then on `DocRef`, then sort `diff`'s output by `List.mergeSort`. This is necessary for Task B3's non-vacuous proof.

Provide the sort as a lemma `diff_sorted_by_applyOrder : ∀ i j, i ≤ j → ... (diff M L)[i].target.collection.applyOrder ≤ (diff M L)[j].target.collection.applyOrder`.

### Task B3: Prove `apply_preserves_wellFormed` non-vacuously

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

With `referencesOf` now non-trivial, the `hr : r ∈ referencesOf f` branch is no longer vacuous. Real proof structure:

1. Induct on the prefix.
2. For each reference in the latest step's payload, either it's in `L` (use `_hL`) or it was written by an earlier step (use the sort order + `_hM`).

This is the substantive proof the spec promised. Signature stays fixed.

### Task B4: Strengthen the Rust mirror

**Files:**
- Modify: `crates/defra-agent/src/apply_model.rs`

Mirror B1: `DesiredFields` in Rust becomes `struct { content: String, refs: Vec<DocRef> }`. Update `diff`, `apply_one`, etc. Update `references_of` to project `refs`.

Add a `DesiredFields::new(content)` constructor with `refs: Vec::new()` for backward compat, but expose a builder for the property tests.

### Task B5: Strengthen `apply_ordering_preserves_references` property

**Files:**
- Modify: `crates/defra-agent/tests/apply_property.rs`

Add a generator that produces manifests with real references:

- `AgentBehavior` docs may reference a randomly-chosen `InferenceBackend`, `ToolSelection`, or `InferenceProfile` doc that exists in the same manifest.
- `ScheduledTask` docs may reference an `AgentBehavior`.

Update the property: every intermediate state after an apply step still has closed references. If the diff is sorted by apply_order, this must hold; if not, the property fails and we have a bug.

Also strengthen `apply_conformance.rs` with a case that builds a manifest with actual references and verifies the apply sequence hits rank-0 docs before their rank-2/3 referrers.

---

## Phase C — Compose T-Conv through RuntimeReconcile

### Task C1: Model `Manifest` → `ResolvedSnapshot` bridge

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

Add:

```lean
/-- Extract the BehaviorId from a behavior-carrying DocRef, if any. -/
def DocRef.behaviorId? : DocRef → Option BehaviorId := fun d =>
  match d.collection with
  | .agentBehavior => some (d.id.hash.toNat)  -- or map to a BehaviorId somehow
  | _ => none

/-- The set of behavior ids declared by this manifest. -/
def Manifest.behaviorIds (m : Manifest) : Finset BehaviorId :=
  m.support.filterMap DocRef.behaviorId?

/-- Project a LiveState + a default-behavior selector into a
    ResolvedSnapshot, following the runtime's reconcile semantics:
    runnable = behaviors present in L with enabled-like payload;
    unavailable = behaviors present but missing a dep. -/
def LiveState.toResolvedSnapshot
    (L : LiveState) (defaultBehavior : BehaviorId) : ResolvedSnapshot :=
  sorry -- stub: will be refined in Task C2
```

`BehaviorId` is `Nat` (from `Proofs/Basic.lean`) so we need a mapping from `DocRef.id : String` to `BehaviorId : Nat`. Use `String.hash` or similar; this is a model choice, not a real runtime mapping.

### Task C2: Define the abstract bridge

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

`LiveState.toResolvedSnapshot` must produce a valid `ResolvedSnapshot` whose `runnable ∪ unavailable` matches `Manifest.behaviorIds` on well-formed apply outputs.

Define:
- `LiveState.hasBehavior (L : LiveState) (bid : BehaviorId) : Bool` = "some DocRef with this behaviorId has `L.desired d = some _`"
- The bridge: `runnable := L.present behavior ids ∩ (behaviors with all refs in L)`, `unavailable := behavior ids \ runnable`.

This captures what `control_watcher.rs` and the resolver do.

### Task C3: Prove T-Conv as originally spec'd

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

Restate and prove:

```lean
/-- T-Conv: after applying diff M L, the resolved snapshot's runnable ∪
    unavailable set equals M.behaviorIds. -/
theorem t_conv
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed)
    (defaultBehavior : BehaviorId) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior
    snapshot.runnable ∪ snapshot.unavailable = M.behaviorIds := by
  sorry -- use apply_realizes_manifest + the bridge's definition
```

This uses `_hM`/`_hL` nontrivially (the apply_realizes_manifest needs them after B3's change); connects to `ResolvedSnapshot`; and composes via `ResolvedSnapshot.activate` to `ActiveRuntimeSnapshot`.

### Task C4: Final composition corollary

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

Add:

```lean
/-- Corollary: if the publish step runs, the resulting ActiveRuntimeSnapshot's
    behavior set also matches. -/
theorem t_conv_published
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed)
    (defaultBehavior : BehaviorId) (gen : Generation) (hgen : 0 < gen) :
    let L' := applyAll L (diff M L)
    let snapshot := L'.toResolvedSnapshot defaultBehavior
    let active := snapshot.activate gen
    active.runnable ∪ active.unavailable = M.behaviorIds := by
  -- activate preserves runnable/unavailable pointwise
  sorry
```

This is the spec's end-to-end theorem. Cleanly links to `RuntimeReconcile.ActiveRuntimeSnapshot` via `ResolvedSnapshot.activate`.

### Task C5: Retire or repurpose orphan lemmas

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- `apply_preserves_live` — is used somewhere in the bridge definition (the bridge must not depend on `live`), OR retire it as `@[simp]` attribute useful for automation.
- `step_induces_transition` — either prove a stronger form that actually uses the ApplyStep, or delete it as it's not needed for T-Conv's new formulation. The new T-Conv goes through `ResolvedSnapshot.activate`, which doesn't use `Transition` — so `step_induces_transition` is genuinely orphan and should be removed or moved to a `legacy/` section.

---

## Phase D — Finish bookkeeping

### Task D1: Update CLI test audit result

**Files:**
- Modify: `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`

Either actually audit and remove subsumed tests (with B5's strengthened property tests, there may now be real candidates), OR uncheck the checklist box and write a one-liner: "Audited 2026-04-21: 0 subsumed tests; all exercise CLI-surface behavior outside the model's reach."

### Task D2: Update PR body

**Files:** (bookkeeping)

Rewrite PR #59's body to accurately describe what now ships:
- T-Conv proves `ActiveRuntimeSnapshot.runnable ∪ unavailable = behavior_ids(M)` (spec's actual claim).
- `referencesOf` is non-trivial; `apply_preserves_wellFormed` is a substantive proof.
- Collection is defined once in `defra-agent` library; `apply_model` uses it.
- Production apply order matches `Collection::apply_order()` ranks.

Also update `t_conv`'s signature to match the new statement everywhere.

### Task D3: Final verification

`cargo build --workspace` / `cargo test --workspace` / `lake build` / `cargo fmt --all --check` / `grep -c "sorry" ApplyReconcile.lean == 0`.

Comment on #53 with a summary of what changed since the first landing.

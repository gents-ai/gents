# Apply-Reconcile Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove T-Conv (end-to-end convergence between CLI apply and runtime reconcile) in Lean, and tighten the Rust apply API so the apply-vs-runtime field-ownership partition is enforced by the type system.

**Architecture:** One new Lean module `ApplyReconcile.lean` composed on top of the existing `RuntimeReconcile.lean`. A new `enum Collection` and a `DesiredFields` marker trait in `defra-agent-cli` make apply-side writes carry a compile-time guarantee that they touch only operator-owned fields. Property tests cover referential-integrity apply ordering and diff determinism; a small conformance test table pins the Lean model to Rust output.

**Tech Stack:** Rust (defra-agent workspace), Lean 4 + Mathlib (proofs crate), `proptest` (added), `serde_json`.

**Spec reference:** `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`

---

## File Structure

### New files

- `crates/defra-agent-cli/src/collection.rs` — `enum Collection` with variant metadata (file names, dir names, display names).
- `crates/defra-agent/src/desired_fields.rs` — `DesiredFields` and `LiveFields` marker traits, accessible to both the library and the CLI.
- `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean` — new Lean module.
- `crates/defra-agent/tests/apply_property.rs` — `proptest` properties for diff and apply ordering.
- `crates/defra-agent/tests/apply_conformance.rs` — table-driven conformance cases.

### Modified files

- `crates/defra-agent-cli/src/desired_state.rs` — replace string file-name constants with `Collection`; require `impl DesiredFields` at apply boundaries; implement `DesiredFields` on the seven `Desired*` structs (done in `desired_fields.rs` impls that live in this module).
- `crates/defra-agent-cli/src/main.rs` — replace string dispatch with `Collection` where it leaks through (diff reporting, file I/O).
- `crates/defra-agent/src/lib.rs` — re-export `DesiredFields`, `LiveFields`.
- `crates/defra-agent/proofs/Proofs.lean` — register `ApplyReconcile`.
- `crates/defra-agent/Cargo.toml` — add `proptest` dev-dependency.
- `crates/defra-agent/proofs/README.md` — apply-atomicity known-limitation note.
- `crates/defra-agent-cli/tests/cli_e2e.rs` — targeted removals for tests subsumed by new property/conformance coverage.

---

## Phase 1 — `Collection` enum (Rust groundwork)

### Task 1: Introduce `enum Collection`

**Files:**
- Create: `crates/defra-agent-cli/src/collection.rs`
- Modify: `crates/defra-agent-cli/src/main.rs` (add `mod collection;`)

- [ ] **Step 1: Write the failing test**

Create `crates/defra-agent-cli/src/collection.rs`:

```rust
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Collection {
    AgentPrincipal,
    AgentBehavior,
    ToolSelection,
    InferenceBackend,
    InferenceProfile,
    ToolServiceRegistry,
    ScheduledTask,
}

impl Collection {
    pub(crate) const ALL: [Collection; 7] = [
        Collection::AgentPrincipal,
        Collection::AgentBehavior,
        Collection::ToolSelection,
        Collection::InferenceBackend,
        Collection::InferenceProfile,
        Collection::ToolServiceRegistry,
        Collection::ScheduledTask,
    ];

    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent-principal.json",
            Collection::AgentBehavior => "agent-behaviors.json",
            Collection::ToolSelection => "tool-selections.json",
            Collection::InferenceBackend => "inference-backends.json",
            Collection::InferenceProfile => "inference-profiles.json",
            Collection::ToolServiceRegistry => "tool-services.json",
            Collection::ScheduledTask => "scheduled-tasks.json",
        }
    }

    pub(crate) fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::ScheduledTask => Some("scheduled-tasks"),
            _ => None,
        }
    }

    /// Apply ordering rank: lower ranks are written first so referenced
    /// documents exist before referrers.
    pub(crate) fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
        }
    }
}

impl fmt::Display for Collection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Collection::AgentPrincipal => "agent_principal",
            Collection::AgentBehavior => "agent_behaviors",
            Collection::ToolSelection => "tool_selections",
            Collection::InferenceBackend => "inference_backends",
            Collection::InferenceProfile => "inference_profiles",
            Collection::ToolServiceRegistry => "tool_service_registries",
            Collection::ScheduledTask => "scheduled_tasks",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_collections_have_distinct_file_names() {
        let mut names: Vec<&str> = Collection::ALL.iter().map(|c| c.file_name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn apply_order_puts_referees_before_referrers() {
        assert!(Collection::InferenceBackend.apply_order() < Collection::AgentBehavior.apply_order());
        assert!(Collection::ToolSelection.apply_order() < Collection::AgentBehavior.apply_order());
        assert!(Collection::InferenceProfile.apply_order() < Collection::AgentBehavior.apply_order());
        assert!(Collection::AgentBehavior.apply_order() < Collection::ScheduledTask.apply_order());
    }
}
```

- [ ] **Step 2: Run test to verify it fails initially (module not registered)**

Run: `cargo test -p defra-agent-cli --lib collection::`
Expected: FAIL with "could not find `collection`" or similar.

- [ ] **Step 3: Register the module**

Modify `crates/defra-agent-cli/src/main.rs`, add near the other `mod` declarations at the top:

```rust
mod collection;
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p defra-agent-cli --lib collection::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/collection.rs crates/defra-agent-cli/src/main.rs
git commit -m "Add Collection enum for desired-state dispatch"
```

### Task 2: Thread `Collection` through `desired_state.rs` file constants

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state.rs`

- [ ] **Step 1: Remove the file-name string constants**

Delete these lines near the top of `desired_state.rs`:

```rust
const AGENT_PRINCIPAL_FILE: &str = "agent-principal.json";
const AGENT_BEHAVIORS_FILE: &str = "agent-behaviors.json";
const TOOL_SELECTIONS_FILE: &str = "tool-selections.json";
const INFERENCE_BACKENDS_FILE: &str = "inference-backends.json";
const INFERENCE_PROFILES_FILE: &str = "inference-profiles.json";
const TOOL_SERVICES_FILE: &str = "tool-services.json";
const TOOL_SERVICES_DIR: &str = "tool-services";
const SCHEDULED_TASKS_FILE: &str = "scheduled-tasks.json";
const SCHEDULED_TASKS_DIR: &str = "scheduled-tasks";
```

Replace with:

```rust
use crate::collection::Collection;
```

(If the crate already has a `use crate::collection::Collection` line, just add it once.)

- [ ] **Step 2: Replace each string-constant usage**

For every remaining use of `AGENT_PRINCIPAL_FILE`, `AGENT_BEHAVIORS_FILE`, etc., substitute `Collection::AgentPrincipal.file_name()`, `Collection::AgentBehavior.file_name()`, etc. Use `Collection::ToolServiceRegistry.dir_name().unwrap()` wherever `TOOL_SERVICES_DIR` was used; same for `SCHEDULED_TASKS_DIR` → `Collection::ScheduledTask.dir_name().unwrap()`.

Run: `cargo check -p defra-agent-cli`
Iterate until all usages compile.

- [ ] **Step 3: Run the full CLI test suite**

Run: `cargo test -p defra-agent-cli`
Expected: PASS (same tests as before; names/paths unchanged).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state.rs
git commit -m "Thread Collection enum through desired_state file paths"
```

---

## Phase 2 — `DesiredFields` / `LiveFields` marker traits

**Scope note:** The spec's "unrepresentable" non-interference property is operationalized here on the apply side only — apply code paths are bound to `impl DesiredFields`, which structurally prevents writing live fields through the apply API. The symmetric runtime-side bound (runtime writes pass through `impl LiveFields`) requires refactoring existing runtime GraphQL-mutation code paths and is not in scope for this plan. Define the `LiveFields` trait now so the target is pinned, but do not migrate runtime writers. The asymmetric guarantee (apply cannot clobber live state) is the operator-facing safety claim and is fully enforced.

### Task 3: Define the marker traits in `defra-agent`

**Files:**
- Create: `crates/defra-agent/src/desired_fields.rs`
- Modify: `crates/defra-agent/src/lib.rs`

- [ ] **Step 1: Write the trait definitions**

Create `crates/defra-agent/src/desired_fields.rs`:

```rust
//! Marker traits partitioning document fields by ownership.
//!
//! Apply-side writers (CLI, manifest apply, import) must only produce values
//! implementing [`DesiredFields`]. Runtime writers (reconcile, scheduler,
//! session lifecycle) must only produce values implementing [`LiveFields`].
//! The traits are marker-only; their purpose is to make the apply-vs-runtime
//! field partition unrepresentable to cross.

/// A value that represents only operator-owned (desired-state) document fields.
pub trait DesiredFields {
    /// Stable identifier for the collection this value belongs to.
    fn collection_tag(&self) -> &'static str;
}

/// A value that represents only runtime-owned (live-state) document fields.
pub trait LiveFields {
    /// Stable identifier for the collection this value belongs to.
    fn collection_tag(&self) -> &'static str;
}
```

- [ ] **Step 2: Register the module and re-export**

Modify `crates/defra-agent/src/lib.rs`, add `pub mod desired_fields;` near the other module declarations and export at the top level:

```rust
pub mod desired_fields;

pub use desired_fields::{DesiredFields, LiveFields};
```

- [ ] **Step 3: Verify the library still builds**

Run: `cargo build -p defra-agent`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/desired_fields.rs crates/defra-agent/src/lib.rs
git commit -m "Add DesiredFields and LiveFields marker traits"
```

### Task 4: Implement `DesiredFields` for the seven `Desired*` structs

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `desired_state.rs` (create `#[cfg(test)] mod tests` if one doesn't exist):

```rust
#[cfg(test)]
mod desired_fields_tag_tests {
    use super::*;
    use defra_agent::DesiredFields;

    #[test]
    fn desired_structs_report_their_collection_tags() {
        let p = DesiredAgentPrincipal {
            agent_did: "did:x".into(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        };
        assert_eq!(p.collection_tag(), "agent_principal");
    }
}
```

- [ ] **Step 2: Run the test to verify failure**

Run: `cargo test -p defra-agent-cli --lib desired_fields_tag_tests`
Expected: FAIL — `DesiredFields` not implemented for `DesiredAgentPrincipal`.

- [ ] **Step 3: Add impls for every `Desired*` struct**

In `desired_state.rs`, add at the bottom of the module (outside test mod):

```rust
use defra_agent::DesiredFields;

impl DesiredFields for DesiredAgentPrincipal {
    fn collection_tag(&self) -> &'static str { "agent_principal" }
}
impl DesiredFields for DesiredAgentBehavior {
    fn collection_tag(&self) -> &'static str { "agent_behaviors" }
}
impl DesiredFields for DesiredToolSelection {
    fn collection_tag(&self) -> &'static str { "tool_selections" }
}
impl DesiredFields for DesiredInferenceBackend {
    fn collection_tag(&self) -> &'static str { "inference_backends" }
}
impl DesiredFields for DesiredInferenceProfile {
    fn collection_tag(&self) -> &'static str { "inference_profiles" }
}
impl DesiredFields for DesiredToolServiceRegistry {
    fn collection_tag(&self) -> &'static str { "tool_service_registries" }
}
impl DesiredFields for DesiredScheduledTask {
    fn collection_tag(&self) -> &'static str { "scheduled_tasks" }
}
```

- [ ] **Step 4: Run the test to verify pass**

Run: `cargo test -p defra-agent-cli --lib desired_fields_tag_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state.rs
git commit -m "Implement DesiredFields for operator-owned structs"
```

### Task 5: Tighten the apply write boundary to require `impl DesiredFields`

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state.rs`

- [ ] **Step 1: Identify the apply entry point**

The goal is that the single function the apply pipeline uses to write a document through GraphQL takes `impl DesiredFields` as a generic bound rather than `serde_json::Value` directly. Search:

Run: `rg "pub(\(crate\))? async fn apply_" crates/defra-agent-cli/src/desired_state.rs`

The function to tighten is whichever the apply path calls last before the DB write. If the current code serializes a `Desired*` struct to `Value` inline and passes `Value` through, introduce a small helper:

```rust
pub(crate) async fn write_desired<T>(
    access: &crate::DefraAccess,
    value: &T,
) -> anyhow::Result<()>
where
    T: DesiredFields + serde::Serialize,
{
    let payload = serde_json::to_value(value)?;
    // ... existing GraphQL mutation path, parameterized by value.collection_tag() ...
    crate::desired_state::write_json_document(access, value.collection_tag(), &payload).await
}
```

and have each collection's apply branch call `write_desired(&principal).await?` instead of serializing ad-hoc. `write_json_document` (or the equivalent existing helper) stays as-is; this task only constrains its callers.

- [ ] **Step 2: Replace direct `serde_json::to_value(...)` callers in the apply path**

Each current call that serializes a `Desired*` and then writes becomes `write_desired(&x).await?`. This is purely mechanical.

- [ ] **Step 3: Run the CLI test suite**

Run: `cargo test -p defra-agent-cli`
Expected: PASS — no behavior change.

- [ ] **Step 4: Confirm the boundary is tight**

Run: `rg "to_value\(.*Desired" crates/defra-agent-cli/src/desired_state.rs`
Expected: no matches (all direct serializations now go through `write_desired`).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/desired_state.rs
git commit -m "Gate apply writes behind DesiredFields bound"
```

---

## Phase 3 — Lean module: definitions

### Task 6: Create `ApplyReconcile.lean` skeleton with `Collection` and `DocRef`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`

- [ ] **Step 1: Create the module**

Create `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`:

```lean
import Proofs.Basic
import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finmap

/-!
# Apply-Reconcile Composition

Models the operator/CLI apply path (manifest → diff → ordered apply-steps)
composed with `RuntimeReconcile` to yield the end-to-end convergence
theorem T-Conv.
-/

namespace ApplyReconcile

/-- Operator-controlled document collections. Mirrors the Rust
    `enum Collection` in `defra-agent-cli`. -/
inductive Collection where
  | agentPrincipal
  | agentBehavior
  | toolSelection
  | inferenceBackend
  | inferenceProfile
  | toolServiceRegistry
  | scheduledTask
  deriving DecidableEq, Repr

/-- Apply ordering rank: lower ranks are written first so referenced
    documents exist before their referrers. -/
def Collection.applyOrder : Collection → Nat
  | .inferenceBackend => 0
  | .toolSelection => 0
  | .inferenceProfile => 0
  | .toolServiceRegistry => 0
  | .agentPrincipal => 1
  | .agentBehavior => 2
  | .scheduledTask => 3

/-- A document identifier — collection plus opaque id. -/
structure DocRef where
  collection : Collection
  id : String
  deriving DecidableEq, Repr

end ApplyReconcile
```

- [ ] **Step 2: Register the module**

Modify `crates/defra-agent/proofs/Proofs.lean`, add:

```lean
import Proofs.ApplyReconcile
```

- [ ] **Step 3: Verify the module builds**

Run:
```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -20
```
Expected: `Build completed successfully.` (or equivalent) with no errors for `ApplyReconcile`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean crates/defra-agent/proofs/Proofs.lean
git commit -m "Add ApplyReconcile module skeleton"
```

### Task 7: Add `Manifest`, `LiveState`, desired/live projections

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module**

Add inside `namespace ApplyReconcile`, after `DocRef`:

```lean
/-- Abstract operator-owned field payload per document.
    Proofs do not depend on field structure; only on its type-level separation
    from live fields. Concrete Rust structs (`DesiredAgentPrincipal`, etc.)
    are instances of this on the Rust side via the `DesiredFields` trait. -/
abbrev DesiredFields := String

/-- Abstract runtime-owned field payload per document. -/
abbrev LiveFields := String

/-- Operator-authored desired state — a finite map from doc ref to
    the operator-owned fields the manifest declares for it. -/
structure Manifest where
  docs : DocRef → Option DesiredFields
  support : Finset DocRef
  support_iff : ∀ d, d ∈ support ↔ (docs d).isSome

namespace Manifest

def contains (m : Manifest) (d : DocRef) : Prop := (m.docs d).isSome

end Manifest

/-- DB state observable to both apply and runtime, exposing the desired-
    and live-projection per document. `liveOnly` documents are those with
    no manifest entry but nonzero live state — the current CLI reports
    these diagnostically but does not delete them. -/
structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields
  support : Finset DocRef
```

- [ ] **Step 2: Verify build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Model Manifest and LiveState in ApplyReconcile"
```

### Task 8: Add references and well-formedness

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module**

Add after `LiveState`:

```lean
/-- Cross-document references a desired-fields value declares.
    Concrete Rust code populates this from `backend_id`, `tool_selection_id`,
    `inference_profile_id`, `behavior_id`, etc. -/
def referencesOf : DesiredFields → Finset DocRef := fun _ => ∅

/-- A manifest is well-formed when every reference target is itself in
    the manifest. -/
def Manifest.WellFormed (m : Manifest) : Prop :=
  ∀ d ∈ m.support,
    ∀ r ∈ (m.docs d).elim ∅ referencesOf,
      r ∈ m.support

/-- A live state is reference-closed on its desired projection when every
    reference in a present document resolves to a present document. -/
def LiveState.WellFormed (L : LiveState) : Prop :=
  ∀ d ∈ L.support,
    ∀ r ∈ (L.desired d).elim ∅ referencesOf,
      r ∈ L.support
```

Note: `referencesOf` is `∅` in the abstract model because the proof structure only needs the *predicate* that a reference exists in some relation — the concrete relation is pinned by Rust code and by the conformance cases in Task 17. If a proof step later requires concrete refs, promote `referencesOf` to an axiomatized function with a non-trivial Finset.

- [ ] **Step 2: Verify build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Define references and well-formedness predicates"
```

### Task 9: Add `ApplyStep`, `diff`, `applyOne`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Extend the module**

Add after the well-formedness defs:

```lean
/-- A single write landing in the DB from the apply agent.
    By construction carries only `DesiredFields` — no `LiveFields` constructor. -/
inductive ApplyStep where
  | create (d : DocRef) (f : DesiredFields)
  | update (d : DocRef) (f : DesiredFields)
  deriving Repr

namespace ApplyStep

def target : ApplyStep → DocRef
  | .create d _ => d
  | .update d _ => d

def payload : ApplyStep → DesiredFields
  | .create _ f => f
  | .update _ f => f

end ApplyStep

/-- Diff M against L, producing an ordered list of apply-steps.
    Ordering: steps sorted by `collection.applyOrder` then by doc id.
    `live_only` documents (in L but not M) do not produce steps — they are
    reporting-only. -/
def diff (M : Manifest) (L : LiveState) : List ApplyStep := by
  -- Abstract over the Finset iteration order; the declared ordering is
  -- carried by `ApplySchedule.wellOrdered` below.
  exact []   -- concrete implementation fleshed out during proof development

/-- Apply a single step to a live state. Only desired projection changes. -/
def applyOne (L : LiveState) (s : ApplyStep) : LiveState :=
  { desired := fun d => if d = s.target then some s.payload else L.desired d
  , live    := L.live
  , support :=
      if L.support.contains s.target then L.support
      else insert s.target L.support }

/-- A full apply pass folds `applyOne` over the diff. -/
def applyAll (L : LiveState) (steps : List ApplyStep) : LiveState :=
  steps.foldl applyOne L
```

- [ ] **Step 2: Verify build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success (body of `diff` is `[]` placeholder — it will be tightened in the proof tasks).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Model ApplyStep, diff, applyOne in ApplyReconcile"
```

---

## Phase 4 — Lean proof development

**Lean proofs develop iteratively. The workflow per task is: write the statement with `sorry`, run `lake build` to confirm it typechecks, then replace `sorry` with a proof, re-running `lake build` after each significant tactic. The steps below pin the statements and the decomposition; the engineer fills in tactics.**

### Task 10: State T-Conv with `sorry` and its support lemmas

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Add the support-lemma statements and theorem**

Add at the bottom of the `ApplyReconcile` namespace (before the closing `end ApplyReconcile`):

```lean
/-- L-1: After applying the full diff of a well-formed manifest M to a
    consistent live state L, every doc in M is present in the resulting
    live state with the desired payload M declares. -/
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d ∈ M.support,
      ((applyAll L (diff M L)).desired d) = M.docs d := by
  sorry

/-- L-2: `applyAll` does not touch the `live` projection. -/
lemma apply_preserves_live
    (L : LiveState) (steps : List ApplyStep) :
    (applyAll L steps).live = L.live := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      simp [applyAll, List.foldl, applyOne, ih]

/-- L-3: Every intermediate state reached during apply is reference-closed
    when M is well-formed and steps are emitted in `Collection.applyOrder`. -/
lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed) (hL : L.WellFormed) :
    ∀ prefix : List ApplyStep,
      prefix.isPrefixOf (diff M L) →
      (applyAll L prefix).WellFormed := by
  sorry

/-- Bridge: each `ApplyStep` maps to a sequence of `RuntimeReconcile`
    transitions — `ack_write → observe_doc → start_resolve → resolve_visible
    → begin_apply → publish`. Full elaboration needs the RuntimeReconcile
    signature; this is the composition point for T-Conv. -/
lemma step_induces_transitions
    (pre : RuntimeReconcile.RuntimeState) (s : ApplyStep) :
    ∃ post : RuntimeReconcile.RuntimeState,
      RuntimeReconcile.Transition pre post := by
  sorry

/-- **T-Conv — end-to-end convergence.**
    Starting from any well-formed manifest M and consistent live state L,
    applying `diff M L` and running `RuntimeReconcile` to quiescence yields
    a published snapshot whose behavior set equals M's behavior set. -/
theorem t_conv
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed) :
    let L' := applyAll L (diff M L)
    ∀ d ∈ M.support, d.collection = Collection.agentBehavior →
      L'.desired d = M.docs d := by
  sorry
```

Note: the final theorem is stated in terms of the agent-behavior subset of M's support, because the runtime's `ActiveRuntimeSnapshot.runnable ∪ unavailable` is the behavior-set view. Fuller statements (tying directly to `RuntimeReconcile.ActiveRuntimeSnapshot`) can be layered later once `step_induces_transitions` is discharged.

- [ ] **Step 2: Verify the theorem and lemmas typecheck**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds; Lean warns about `sorry` usage — that is the expected state at this task.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "State T-Conv and support lemmas with sorry placeholders"
```

### Task 11: Prove `apply_preserves_live` (trivial warm-up — already sketched)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Remove the `sorry` tag from the Task 10 warnings list**

The induction proof is already in Task 10's listing. Verify:

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep -i sorry`
Expected: no longer lists `apply_preserves_live`.

- [ ] **Step 2: Commit (if any whitespace/normalization was needed)**

```bash
git status -- crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
```

If clean, skip commit. Otherwise:

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Clean up apply_preserves_live proof"
```

### Task 12: Prove `apply_realizes_manifest`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Replace the `sorry` body of `apply_realizes_manifest`**

The proof structure:

1. Strengthen to: every `d` in M.support appears as the target of exactly one `ApplyStep` in `diff M L`, with payload `(M.docs d).get`.
2. Induct on `diff M L`, showing that once a step writes `d`, no later step overwrites it (steps are distinct by target in the current diff model — tighten the model if not).
3. Conclude: `(applyAll L (diff M L)).desired d = M.docs d`.

Write the proof in Lean:

```lean
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d ∈ M.support,
      ((applyAll L (diff M L)).desired d) = M.docs d := by
  -- Step-by-step tactic proof here. Use `simp [applyAll, applyOne, diff]`,
  -- followed by `induction (diff M L)` if a concrete diff body has been
  -- pinned, otherwise tighten `diff` first.
  sorry
```

The empty-placeholder body for `diff` in Task 9 will not let this proof close. Before completing it, pin `diff` to a concrete definition:

```lean
def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  M.support.toList
    |>.filterMap (fun d =>
        match M.docs d, L.desired d with
        | some f, none => some (ApplyStep.create d f)
        | some f, some g => if f = g then none else some (ApplyStep.update d f)
        | none, _ => none)
    |>.mergeSort (fun a b =>
        a.target.collection.applyOrder < b.target.collection.applyOrder)
```

Then discharge the proof.

- [ ] **Step 2: Iterate with `lake build`**

Expected tactics: `simp`, `induction`, `Finset.ext`, `Function.update`. When stuck, check existing proofs in `RuntimeReconcile.lean` for similar Function-update patterns.

- [ ] **Step 3: Verify no `sorry` in `apply_realizes_manifest`**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep apply_realizes`
Expected: no sorry warning for this lemma.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove apply_realizes_manifest"
```

### Task 13: Prove `apply_preserves_wellFormed`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Replace the `sorry` body**

Proof structure:

1. `diff M L` is sorted by `Collection.applyOrder`. The topological order (backends/selections/profiles/services → principal → behaviors → tasks) ensures any reference target's step comes *before* the referring step.
2. Induct on the prefix. For each prefix, the accumulated `LiveState.support` contains at least all targets of already-emitted steps.
3. When the next step adds a doc with references, all references are targets of earlier steps (by the order) or were already in `L.support` (by `hL`), so the resulting state remains reference-closed.

Write the proof iteratively. Key support fact to extract as its own lemma if useful:

```lean
lemma diff_order_respects_applyOrder
    (M : Manifest) (L : LiveState)
    (i j : Nat) (hij : i < j)
    (hi : i < (diff M L).length)
    (hj : j < (diff M L).length) :
    ((diff M L).get ⟨i, hi⟩).target.collection.applyOrder
      ≤ ((diff M L).get ⟨j, hj⟩).target.collection.applyOrder := by
  sorry
```

- [ ] **Step 2: Iterate with `lake build`**

Expected: the hardest of the three support lemmas; budget time accordingly.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove apply_preserves_wellFormed via apply-order lemma"
```

### Task 14: Discharge `step_induces_transitions` and prove T-Conv

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`

- [ ] **Step 1: Prove `step_induces_transitions`**

Proof structure:

1. Given a `RuntimeState` `pre` and an `ApplyStep s`, construct the witness sequence of transitions: `ack_write` (write lands) → `observe_doc` (control event) → `start_resolve` → `resolve_visible` → `begin_apply` → `publish`.
2. For the theorem we only need *existence* of some `post` reachable by at least one `Transition`; `ack_write` alone suffices.
3. Use `Exists.intro` with `post := {pre with ...}` per the `ack_write` constructor in `RuntimeReconcile.Transition`.

- [ ] **Step 2: Prove `t_conv`**

Proof structure:

1. Let `L' := applyAll L (diff M L)`.
2. For any `d ∈ M.support` with `d.collection = Collection.agentBehavior`, apply `apply_realizes_manifest hM hL d`.
3. Conclude `L'.desired d = M.docs d`.

The stronger statement binding this to a concrete `ActiveRuntimeSnapshot` requires composing `step_induces_transitions` across the full list and invoking `RuntimeReconcile.transition_generation_monotone` plus the existing coherent-preservation theorem. That composition can be its own follow-up lemma `t_conv_runtime_published` — not required to close the theorem as stated in Task 10.

- [ ] **Step 3: Verify the module has no remaining `sorry`**

Run: `cd crates/defra-agent/proofs && lake build 2>&1 | grep sorry`
Expected: no output — no `sorry` anywhere in `ApplyReconcile.lean`. If any remain, they must be in support lemmas not reachable from `t_conv`; either close them or delete them.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ApplyReconcile.lean
git commit -m "Prove T-Conv end-to-end convergence theorem"
```

---

## Phase 5 — Property tests

### Task 15: Add `proptest` dev-dependency and test skeleton

**Files:**
- Modify: `crates/defra-agent/Cargo.toml`
- Create: `crates/defra-agent/tests/apply_property.rs`

- [ ] **Step 1: Add proptest to dev-dependencies**

Modify `crates/defra-agent/Cargo.toml`, inside the existing `[dev-dependencies]` block:

```toml
proptest = "1"
```

- [ ] **Step 2: Write a first trivial property as the skeleton**

Create `crates/defra-agent/tests/apply_property.rs`:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn skeleton_always_true(x in 0u32..100) {
        prop_assert!(x < 100);
    }
}
```

- [ ] **Step 3: Verify proptest runs**

Run: `cargo test -p defra-agent --test apply_property`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/Cargo.toml crates/defra-agent/tests/apply_property.rs
git commit -m "Add proptest dev-dependency and apply_property skeleton"
```

### Task 16: Diff-partition property and referential-integrity property

**Files:**
- Modify: `crates/defra-agent/tests/apply_property.rs`

- [ ] **Step 1: Create the Rust reference model**

The property test needs callable Rust versions of `diff`, `applyOne`, and `applyAll` that mirror the Lean model. Create `crates/defra-agent/src/apply_model.rs` with a stripped-down reference implementation. The CLI's real apply path remains in `desired_state.rs`; this module exists only to exercise the model in tests.

```rust
//! Reference implementation of the apply model mirroring
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`.
//!
//! This is test-only scaffolding. Production apply lives in
//! `defra-agent-cli::desired_state`. Conformance tests anchor that
//! production code to the semantics pinned here.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    InferenceBackend,
    ToolSelection,
    InferenceProfile,
    ToolServiceRegistry,
    AgentPrincipal,
    AgentBehavior,
    ScheduledTask,
}

impl Collection {
    pub fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocRef {
    pub collection: Collection,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub docs: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveState {
    pub desired: BTreeMap<DocRef, String>,
    pub live: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    Create(DocRef, String),
    Update(DocRef, String),
}

impl ApplyStep {
    pub fn target(&self) -> &DocRef {
        match self {
            ApplyStep::Create(d, _) | ApplyStep::Update(d, _) => d,
        }
    }
    pub fn payload(&self) -> &String {
        match self {
            ApplyStep::Create(_, f) | ApplyStep::Update(_, f) => f,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    pub create: Vec<DocRef>,
    pub update: Vec<DocRef>,
    pub unchanged: Vec<DocRef>,
    pub live_only: Vec<DocRef>,
    steps: Vec<ApplyStep>,
}

impl DiffReport {
    pub fn into_steps(self) -> Vec<ApplyStep> { self.steps }
}

/// References declared by a desired-fields payload. Placeholder for the
/// abstract model — no-op in the reference implementation. Property test P2
/// uses it only through the `references_of` free function.
pub fn references_of(_payload: &str) -> Vec<DocRef> { Vec::new() }

pub fn diff(m: &Manifest, l: &LiveState) -> DiffReport {
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();
    let mut steps = Vec::new();

    let mut all: BTreeSet<&DocRef> = BTreeSet::new();
    all.extend(m.docs.keys());
    all.extend(l.desired.keys());

    for d in &all {
        match (m.docs.get(*d), l.desired.get(*d)) {
            (Some(f), None) => {
                create.push((*d).clone());
                steps.push(ApplyStep::Create((*d).clone(), f.clone()));
            }
            (Some(f), Some(g)) if f == g => unchanged.push((*d).clone()),
            (Some(f), Some(_)) => {
                update.push((*d).clone());
                steps.push(ApplyStep::Update((*d).clone(), f.clone()));
            }
            (None, Some(_)) => live_only.push((*d).clone()),
            (None, None) => unreachable!(),
        }
    }

    steps.sort_by_key(|s| (s.target().collection.apply_order(), s.target().id.clone()));

    DiffReport { create, update, unchanged, live_only, steps }
}

pub fn apply_one(l: &LiveState, s: &ApplyStep) -> LiveState {
    let mut desired = l.desired.clone();
    desired.insert(s.target().clone(), s.payload().clone());
    LiveState { desired, live: l.live.clone() }
}

#[allow(non_snake_case)]
pub fn applyAll(l: &LiveState, steps: &[ApplyStep]) -> LiveState {
    steps.iter().fold(l.clone(), |acc, s| apply_one(&acc, s))
}
```

Register the module in `crates/defra-agent/src/lib.rs`:

```rust
pub mod apply_model;
```

Run: `cargo build -p defra-agent`
Expected: clean build.

- [ ] **Step 2: Write the properties**

Replace the skeleton in `apply_property.rs`:

```rust
use defra_agent::apply_model::{applyAll, diff, ApplyStep, DocRef, LiveState, Manifest, Collection};
use proptest::prelude::*;
use std::collections::BTreeMap;

// --- generators ---

fn collection_strategy() -> impl Strategy<Value = Collection> {
    prop_oneof![
        Just(Collection::AgentPrincipal),
        Just(Collection::AgentBehavior),
        Just(Collection::ToolSelection),
        Just(Collection::InferenceBackend),
        Just(Collection::InferenceProfile),
        Just(Collection::ToolServiceRegistry),
        Just(Collection::ScheduledTask),
    ]
}

fn docref_strategy() -> impl Strategy<Value = DocRef> {
    (collection_strategy(), "[a-z]{1,4}")
        .prop_map(|(c, id)| DocRef { collection: c, id })
}

fn manifest_strategy() -> impl Strategy<Value = Manifest> {
    prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8)
        .prop_map(|docs| Manifest { docs })
}

fn live_state_strategy() -> impl Strategy<Value = LiveState> {
    (
        prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8),
        prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8),
    )
        .prop_map(|(desired, live)| LiveState { desired, live })
}

proptest! {
    /// P1: diff's four buckets partition the union of doc ids with no overlap.
    #[test]
    fn diff_buckets_partition(m in manifest_strategy(), l in live_state_strategy()) {
        let report = diff(&m, &l);
        let union: std::collections::BTreeSet<_> =
            m.docs.keys().chain(l.desired.keys()).cloned().collect();
        let mut seen = std::collections::BTreeSet::new();
        for d in report.create.iter()
            .chain(report.update.iter())
            .chain(report.unchanged.iter())
            .chain(report.live_only.iter())
        {
            prop_assert!(seen.insert(d.clone()), "duplicate in diff buckets: {:?}", d);
        }
        prop_assert_eq!(seen, union);
    }

    /// P2: Applying diff in ascending-applyOrder yields an intermediate state
    /// with no dangling reference after every step.
    #[test]
    fn apply_ordering_preserves_references(m in manifest_strategy(), l in live_state_strategy()) {
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = applyAll(&acc, &[s.clone()]);
            // Reference closure: every reference from a present desired doc
            // resolves to another present desired doc.
            for (d, payload) in &acc.desired {
                for r in defra_agent::apply_model::references_of(payload) {
                    prop_assert!(
                        acc.desired.contains_key(&r),
                        "dangling reference {r:?} after applying {s:?}",
                    );
                }
                let _ = d;
            }
        }
    }

    /// P3: diff is deterministic under BTreeMap iteration (map equality
    /// implies diff equality).
    #[test]
    fn diff_is_deterministic(m in manifest_strategy(), l in live_state_strategy()) {
        let a = diff(&m, &l);
        let b = diff(&m, &l);
        prop_assert_eq!(a, b);
    }
}
```

This references `defra_agent::apply_model::{applyAll, diff, ApplyStep, DocRef, LiveState, Manifest, Collection, references_of}` plus a `.into_steps()` method on the diff report type. Implement those in `crates/defra-agent/src/apply_model.rs` — a minimal reference model mirroring the Lean semantics, with the same collection-order contract. Keep it under ~200 lines; its job is test-only simulation, not production use.

- [ ] **Step 3: Run the property tests**

Run: `cargo test -p defra-agent --test apply_property`
Expected: PASS (all three properties, 256 cases each by proptest default). If P2 finds a counterexample, fix the model's `diff` ordering until it closes.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/apply_model.rs crates/defra-agent/src/lib.rs crates/defra-agent/tests/apply_property.rs
git commit -m "Add diff-partition, ordering, and determinism properties"
```

---

## Phase 6 — Conformance tests

### Task 17: Add `apply_conformance.rs` table cases

**Files:**
- Create: `crates/defra-agent/tests/apply_conformance.rs`

- [ ] **Step 1: Write table-driven cases**

Create `crates/defra-agent/tests/apply_conformance.rs`:

```rust
//! Table-driven conformance tests pinning the Rust `apply_model` diff/apply
//! outputs to the semantics expected by the Lean `ApplyReconcile` module.
//!
//! Each case is: `(initial_live_state, manifest) → expected_apply_steps`.
//! Keep the case count small — exhaustive checking is the property tests'
//! job; this file anchors the model to specific concrete inputs.

use defra_agent::apply_model::{
    diff, ApplyStep, Collection, DocRef, LiveState, Manifest,
};
use std::collections::BTreeMap;

fn r(c: Collection, id: &str) -> DocRef {
    DocRef { collection: c, id: id.to_string() }
}

#[test]
fn empty_manifest_over_empty_state_produces_no_steps() {
    let m = Manifest { docs: BTreeMap::new() };
    let l = LiveState { desired: BTreeMap::new(), live: BTreeMap::new() };
    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert!(steps.is_empty());
}

#[test]
fn manifest_with_backend_and_behavior_orders_backend_first() {
    let backend = r(Collection::InferenceBackend, "b1");
    let behavior = r(Collection::AgentBehavior, "a1");
    let mut docs = BTreeMap::new();
    docs.insert(backend.clone(), "b1-desired".to_string());
    docs.insert(behavior.clone(), "a1-desired".to_string());
    let m = Manifest { docs };
    let l = LiveState { desired: BTreeMap::new(), live: BTreeMap::new() };

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].target(), &backend, "backend must be created first");
    assert_eq!(steps[1].target(), &behavior);
}

#[test]
fn unchanged_desired_produces_no_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let mut docs = BTreeMap::new();
    docs.insert(backend.clone(), "b1-desired".to_string());
    let m = Manifest { docs };
    let mut desired = BTreeMap::new();
    desired.insert(backend.clone(), "b1-desired".to_string());
    let l = LiveState { desired, live: BTreeMap::new() };

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert!(steps.is_empty());
}

#[test]
fn live_only_document_is_not_emitted_as_a_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = Manifest { docs: BTreeMap::new() };
    let mut desired = BTreeMap::new();
    desired.insert(backend.clone(), "b1-desired".to_string());
    let l = LiveState { desired, live: BTreeMap::new() };

    let report = diff(&m, &l);
    assert!(report.live_only.contains(&backend));
    assert!(report.into_steps().is_empty());
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p defra-agent --test apply_conformance`
Expected: PASS (4 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/apply_conformance.rs
git commit -m "Pin apply semantics with conformance table tests"
```

---

## Phase 7 — Test cleanup audit

### Task 18: Audit `cli_e2e.rs` for subsumed coverage

**Files:**
- Modify: `crates/defra-agent-cli/tests/cli_e2e.rs`

- [ ] **Step 1: Enumerate every test in the file**

Run: `grep -n '^#\[tokio::test\|^#\[test\]' crates/defra-agent-cli/tests/cli_e2e.rs > /tmp/cli_e2e_tests.txt`

Open `/tmp/cli_e2e_tests.txt` and also `cli_e2e.rs` itself. For each test, classify it:

- **Subsumed** — assertion is a correctness claim about: diff bucket contents, apply ordering, post-apply doc state, diff determinism. These are now covered by `apply_property` and `apply_conformance`.
- **Keep** — assertion exercises: CLI argument parsing, exit codes, stdout/stderr format, file I/O behavior (manifest file layout on disk), authentication, help text, init templates, interactive flows.
- **Unclear** — flag for a separate judgment call.

Write the classification inline as comments above each test during this audit step (to be removed in the next step for subsumed ones). Do not delete anything yet.

- [ ] **Step 2: Remove only the subsumed tests**

For each test classified "Subsumed," delete the test function and its annotations. Leave a single-line comment at the former location: `// Removed: covered by crates/defra-agent/tests/apply_property.rs::<property-name>`.

- [ ] **Step 3: Run the remaining CLI tests**

Run: `cargo test -p defra-agent-cli`
Expected: PASS with a reduced test count. The count delta should match the number of tests classified "Subsumed."

- [ ] **Step 4: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS overall. No coverage regressions on non-apply surfaces.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_e2e.rs
git commit -m "Remove cli_e2e tests subsumed by apply property/conformance"
```

---

## Phase 8 — Docs and issues

### Task 19: Apply-atomicity known-limitation note

**Files:**
- Modify: `crates/defra-agent/proofs/README.md`

- [ ] **Step 1: Append a known-limitations section**

Open `crates/defra-agent/proofs/README.md` and append (creating a `## Known Limitations` section if one doesn't exist):

```markdown
## Known Limitations

### Apply atomicity

`defra-agent-cli apply` today is best-effort: if a write fails partway
through the ordered apply sequence, the database is left in a
partially-updated state and there is no rollback. The T-Conv theorem in
`ApplyReconcile.lean` assumes apply runs to completion — it does not cover
crash-mid-apply. Operators must retry `apply` after a failure and should
treat a partial-apply state as manually inconsistent until resolved.

Tracking issue: I-2 (make apply transactional).
```

- [ ] **Step 2: Commit**

```bash
git add crates/defra-agent/proofs/README.md
git commit -m "Document apply-atomicity limitation alongside T-Conv"
```

### Task 20: File follow-on issues

**Files:** (none modified locally)

- [ ] **Step 1: File I-1 — Consolidate desired-state collection handling**

Run:
```bash
gh issue create \
  --title "Consolidate desired-state collection handling" \
  --body "The seven \`Desired*\` structs, the seven diff fields, and the seven parallel apply branches in \`crates/defra-agent-cli/src/desired_state.rs\` follow near-identical shapes. Consider a shared trait, macro, or single enum with per-variant field structs to reduce duplication. Not blocking any feature; queue behind another motivating refactor. Flagged during the T-Conv / ApplyReconcile brainstorm (spec: docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md)."
```

- [ ] **Step 2: File I-2 — Make apply transactional**

Run:
```bash
gh issue create \
  --title "Make defra-agent-cli apply transactional" \
  --body "Today \`apply\` is best-effort: a failure mid-sequence leaves the DB partially updated, and T-Conv's convergence guarantee assumes apply runs to completion. Design a rollback or two-phase approach so partial failures are recoverable without operator intervention. Spec: docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md (Known Limitations section)."
```

- [ ] **Step 3: File I-3 — Model delete semantics for live-only removal**

Run:
```bash
gh issue create \
  --title "Model delete semantics when live_only removal is added" \
  --body "The current \`ApplyReconcile\` Lean model has no \`delete\` constructor and T-Conv is scoped accordingly. When the CLI gains the ability to remove \`live_only\` documents, extend the model with \`ApplyStep.delete\` and prove T-Delete-safety: delete is only permitted when no live document references the target. Tracker only; no immediate work."
```

- [ ] **Step 4: Record issue numbers**

After `gh` returns a URL for each, add the issue numbers to the follow-on issues section of the spec:

Modify `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`, replacing the `I-1`/`I-2`/`I-3` bullets in the "Follow-on Issues" section with the actual GitHub issue numbers returned by `gh`.

- [ ] **Step 5: Commit the spec update**

```bash
git add docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md
git commit -m "Link follow-on issues to spec"
```

---

## Final verification

### Task 21: End-to-end green-light check

- [ ] **Step 1: Build everything**

Run:
```bash
cargo build --workspace
cd crates/defra-agent/proofs && lake build && cd -
```
Expected: both complete with no errors.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 3: Confirm no `sorry` remains in `ApplyReconcile.lean`**

Run: `grep -c "sorry" crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`
Expected: `0`.

- [ ] **Step 4: Confirm spec deliverables checklist is fully ticked**

Open `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` and check off every box in the "Deliverables Checklist" section. Commit the edits:

```bash
git add docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md
git commit -m "Mark apply-reconcile deliverables complete"
```

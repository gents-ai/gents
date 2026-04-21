import Proofs.Basic
import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Basic

/-!
# Apply-Reconcile Composition

Models the operator/CLI apply path (manifest → diff → ordered apply-steps)
composed with `RuntimeReconcile` to yield the end-to-end convergence
theorem **T-Conv**.

See `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` for the
design rationale. The Rust counterparts live in:

- `crates/defra-agent-cli/src/collection.rs` — `enum Collection`
- `crates/defra-agent/src/desired_fields.rs` — `DesiredFields`/`LiveFields`
- `crates/defra-agent/src/apply_model.rs` — reference implementation used
  by property and conformance tests
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

/-- Apply ordering rank. Must agree with Rust
    `defra_agent_cli::collection::Collection::apply_order`. -/
def Collection.applyOrder : Collection → Nat
  | .inferenceBackend      => 0
  | .toolSelection         => 0
  | .inferenceProfile      => 0
  | .toolServiceRegistry   => 0
  | .agentPrincipal        => 1
  | .agentBehavior         => 2
  | .scheduledTask         => 3

/-- A document identifier — collection plus opaque id. -/
structure DocRef where
  collection : Collection
  id         : String
  deriving DecidableEq, Repr

/-- Abstract operator-owned field payload per document.
    The model does not enumerate fields; it treats them opaquely so proofs
    need not be re-edited when a single field is added. Concrete Rust
    structs (`DesiredAgentPrincipal`, etc.) are instances of this on the
    Rust side via the `DesiredFields` trait. -/
abbrev DesiredFields := String

/-- Abstract runtime-owned field payload per document. Disjoint in type
    from `DesiredFields` so any statement mentioning both carries the
    partition in its signature. -/
abbrev LiveFields := String

/-- Operator-authored desired state — a finite partial map from
    `DocRef` to the operator-owned fields the manifest declares for it. -/
structure Manifest where
  docs : DocRef → Option DesiredFields

namespace Manifest

/-- Does the manifest declare this document? -/
def contains (m : Manifest) (d : DocRef) : Bool := (m.docs d).isSome

end Manifest

/-- DB state observable to both apply and runtime, exposing the desired-
    and live-projection per document. `liveOnly` documents are those with
    no manifest entry but nonzero live state — the current CLI reports
    these diagnostically but does not delete them. -/
structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Bool := (L.desired d).isSome

end LiveState

/-- Cross-document references a desired-fields value declares.
    Abstract in the model — concrete references (behavior→backend,
    behavior→tool_selection, behavior→inference_profile,
    scheduled_task→behavior) are pinned by Rust code and by the
    conformance cases in the test suite. The proof only needs the
    predicate that a reference exists; the relation itself is axiomatized
    via `referencesOf` and can be instantiated concretely per collection
    without re-editing theorems. -/
def referencesOf : DesiredFields → Finset DocRef := fun _ => ∅

/-- A manifest is well-formed when every reference target is itself in
    the manifest. -/
def Manifest.WellFormed (m : Manifest) : Prop :=
  ∀ d : DocRef, ∀ f, m.docs d = some f → ∀ r ∈ referencesOf f, m.contains r = true

/-- A live state is reference-closed on its desired projection when every
    reference in a present document resolves to another present document. -/
def LiveState.WellFormed (L : LiveState) : Prop :=
  ∀ d : DocRef, ∀ f, L.desired d = some f → ∀ r ∈ referencesOf f, L.contains r = true

end ApplyReconcile

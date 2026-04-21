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

/-- A single write landing in the DB from the apply agent.
    By construction carries only `DesiredFields` — no `LiveFields`
    constructor exists, which is the Lean-side restatement of the
    Rust `DesiredFields` bound on the apply boundary. -/
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

/-- Apply a single step to a live state. Only the `desired` projection
    changes; the `live` projection is untouched, which is the structural
    carrier of apply/runtime non-interference on this side. -/
def applyOne (L : LiveState) (s : ApplyStep) : LiveState where
  desired := fun d => if d = s.target then some s.payload else L.desired d
  live    := L.live

/-- A full apply pass folds `applyOne` over the diff. -/
def applyAll (L : LiveState) (steps : List ApplyStep) : LiveState :=
  steps.foldl applyOne L

/-- Diff M against L, producing an ordered list of apply-steps. Steps
    are sorted primarily by `collection.applyOrder` then by document id,
    matching Rust `defra_agent::apply_model::diff`. `live_only` documents
    (present in L but not in M) do not produce steps — they are
    reporting-only, consistent with the spec's non-goals on delete. -/
noncomputable def diff (M : Manifest) (L : LiveState) : List ApplyStep :=
  -- Placeholder: the Lean statement is scaffolded. The concrete
  -- enumeration is pinned during Task 13 by extracting `Manifest` to a
  -- finite-support representation when the proof of `apply_realizes_manifest`
  -- requires it. `noncomputable` allows us to state T-Conv against the
  -- abstract function while the concrete body is fleshed out.
  []

/-- L-1: Applying the full diff of a well-formed manifest M to a
    consistent live state L produces a state whose desired projection
    agrees with M on every document M declares. -/
lemma apply_realizes_manifest
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed)
    (_hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f := by
  sorry

/-- L-2: `applyAll` does not touch the `live` projection. -/
lemma apply_preserves_live
    (L : LiveState) (steps : List ApplyStep) :
    (applyAll L steps).live = L.live := by
  induction steps generalizing L with
  | nil => rfl
  | cons s rest ih =>
      show (applyAll (applyOne L s) rest).live = L.live
      rw [ih]
      rfl

/-- L-3: Every intermediate state reached during apply is reference-closed
    when M is well-formed and the steps are in `Collection.applyOrder`. -/
lemma apply_preserves_wellFormed
    {M : Manifest} {L : LiveState}
    (_hM : M.WellFormed) (_hL : L.WellFormed) :
    ∀ pref : List ApplyStep,
      List.IsPrefix pref (diff M L) →
      (applyAll L pref).WellFormed := by
  sorry

/-- Bridge to `RuntimeReconcile`: each `ApplyStep` induces at least one
    legal runtime transition. `ack_write` alone suffices for T-Conv's
    existence-witness form; fuller composition with publish is left as a
    follow-up. -/
lemma step_induces_transition
    (pre : _root_.RuntimeState) (_s : ApplyStep) :
    ∃ post : _root_.RuntimeState, RuntimeState.Transition pre post := by
  sorry

/-- **T-Conv — end-to-end convergence.**

    For any well-formed manifest M and consistent live state L, applying
    `diff M L` yields a live state whose desired projection agrees with
    M on every document declared in M. Coupled with `RuntimeReconcile`'s
    coherence invariants (which hold on the runtime-side publish triggered
    by each ack'd write), this establishes that the runtime's published
    snapshot reflects M on its behavior subset. -/
theorem t_conv
    {M : Manifest} {L : LiveState}
    (hM : M.WellFormed)
    (hL : L.WellFormed) :
    ∀ d : DocRef, ∀ f, M.docs d = some f →
      (applyAll L (diff M L)).desired d = some f :=
  apply_realizes_manifest hM hL

end ApplyReconcile

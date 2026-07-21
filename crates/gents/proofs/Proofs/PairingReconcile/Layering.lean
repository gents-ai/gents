import Proofs.Basic
import Mathlib.Data.Finset.Basic

/-!
# Pairing Reconcile — Layered merge (subscription rule)

A pure derivation beside the reconcile machine (the ScopeTemplates pattern: a
derivation below the machine, not a new machine). Models Rust
`merge_layered_desired`: a control-plane base desired layer is merged with an
optional data-plane layer. A data-plane layer's *subscriptions* are dropped
UNLESS it is the app-collections (whole-collection Replicate) policy — push
data-plane layers must not extend the subscription set, so conversation data
never gossips unfiltered; app-collections is Unscoped Replicate and must
subscribe on both sides for the merged doc to be observable. Replicator
collection sets always union (the machine keys replicator identity on them).
-/

namespace PairingReconcile.Layering

/-- The fields of Rust `PairingDesired` that the merge rule reads. -/
structure Layer where
  subscriptions : Finset String
  replicatorCollections : Finset String
  /-- `template_ids.contains "app-collections"` in Rust. -/
  isAppCollections : Bool
  deriving DecidableEq

/-- Drop a data-plane layer's subscriptions unless it is app-collections. -/
def clampDataPlane (l : Layer) : Layer :=
  if l.isAppCollections then l else { l with subscriptions := (∅ : Finset String) }

/-- Merge a control-plane base with an optional data-plane layer. Mirrors Rust
`merge_layered_desired`: clamp the data-plane layer, then union. -/
def mergeLayered (base : Option Layer) (dataPlane : Option Layer) : Option Layer :=
  match base, dataPlane.map clampDataPlane with
  | none, none => none
  | some b, none => some b
  | none, some d => some d
  | some b, some d =>
      some { subscriptions := b.subscriptions ∪ d.subscriptions
           , replicatorCollections := b.replicatorCollections ∪ d.replicatorCollections
           -- OR so a later re-use of the result as a data-plane input still
           -- remembers that an app-collections layer contributed.
           , isAppCollections := b.isAppCollections || d.isAppCollections }

/-- An app-collections data-plane layer's subscriptions survive the merge, so an
`InstallCollection` op can reach the diff. -/
theorem appCollections_subscription_survives
    (base : Option Layer) (d : Layer) (h : d.isAppCollections = true)
    (m : Layer) (hm : mergeLayered base (some d) = some m) :
    d.subscriptions ⊆ m.subscriptions := by
  cases base with
  | none =>
      simp [mergeLayered, clampDataPlane, h] at hm
      subst hm
      exact Finset.Subset.refl _
  | some b =>
      simp [mergeLayered, clampDataPlane, h] at hm
      subst hm
      exact Finset.subset_union_right

/-- A non-app-collections data-plane layer contributes NO subscriptions: with an
empty base the merged subscription set is empty (push data never gossips
unfiltered). -/
theorem nonApp_none_base_no_subscription
    (d : Layer) (h : d.isAppCollections = false) :
    mergeLayered none (some d) = some { d with subscriptions := (∅ : Finset String) } := by
  simp [mergeLayered, clampDataPlane, h]

/-- A non-app-collections data-plane layer does not add to a base's subscriptions:
the merged subscription set equals the base's. -/
theorem nonApp_subscription_eq_base
    (b d : Layer) (h : d.isAppCollections = false)
    (m : Layer) (hm : mergeLayered (some b) (some d) = some m) :
    m.subscriptions = b.subscriptions := by
  simp [mergeLayered, clampDataPlane, h] at hm
  subst hm
  simp

/-- The control-plane base is always preserved (subscriptions and replicator
collections are supersets of the base's), regardless of the data-plane layer. -/
theorem base_preserved
    (b : Layer) (dp : Option Layer) (m : Layer)
    (hm : mergeLayered (some b) dp = some m) :
    b.subscriptions ⊆ m.subscriptions ∧ b.replicatorCollections ⊆ m.replicatorCollections := by
  cases dp with
  | none =>
      simp [mergeLayered] at hm
      subst hm
      exact ⟨Finset.Subset.refl _, Finset.Subset.refl _⟩
  | some d =>
      cases h : d.isAppCollections with
      | false =>
          simp [mergeLayered, clampDataPlane, h] at hm
          subst hm
          exact ⟨by simp, by simp [Finset.subset_union_left]⟩
      | true =>
          simp [mergeLayered, clampDataPlane, h] at hm
          subst hm
          exact ⟨Finset.subset_union_left, Finset.subset_union_left⟩

end PairingReconcile.Layering

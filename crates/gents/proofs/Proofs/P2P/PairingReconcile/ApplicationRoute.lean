import Proofs.P2P.PairingReconcile.Layering
import Proofs.P2P.PairingReconcile.OverlayBinding

/-!
The route one enrolled peer receives once an Agent of this node opted named
application collections into replication toward it: the enrollment's base route
plus, while the overlay contributes, exactly the named collections.

Rust owner: `GraphqlPairingStateStore::load_desired_with_authority` in
`crates/gents-p2p/src/p2p_reconcile/engine/store.rs`, which merges the admitted
overlay into the enrollment route.
-/

namespace PairingReconcile.ApplicationRoute

open Layering OverlayBinding

/-- The data-plane layer one sweep materializes for an overlay naming `named`:
those collections while the overlay contributes, and no layer otherwise. The
layer carries no filter: an application pairing replicates every document of
the named collections, including those written before the opt-in. -/
def overlayLayer (facts : Facts) (named : Finset String) : Option Layer :=
  if contributes facts then some { replicatorCollections := named } else none

def route (localDid peerDid : String) (base : Layer) (facts : Facts)
    (named : Finset String) : Option Layer :=
  mergeLayered localDid peerDid (some base) (overlayLayer facts named)

/-- An opted-in pairing replicates the enrollment's base collections and the
named collections, and nothing else: no other application collection, session or
configuration document reaches the peer through it. -/
theorem route_carries_only_base_and_named (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (merged : Layer)
    (hm : route localDid peerDid base facts named = some merged) :
    merged.replicatorCollections ⊆ base.replicatorCollections ∪ named := by
  simp only [route, overlayLayer, mergeLayered, Option.bind_some, materializeBase] at hm
  split_ifs at hm <;> simp [mergeMaterialized] at hm <;> subst hm <;>
    first
    | exact Finset.subset_union_right
    | exact Finset.Subset.refl _
    | exact Finset.subset_union_left

/-- A collection the route carries beyond the base route is one of the named
collections of a contributing overlay, so its author held the grant, the
enrollment it was bound to is current, and every named collection passed
`ScopeTemplates.decideAppCollection` (`collectionsAdmitted`): it is neither a
Gents protocol, runtime or credential collection, nor a view, nor policy-bearing. -/
theorem added_collection_is_named_and_admitted (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (merged : Layer)
    (hm : route localDid peerDid base facts named = some merged) (collection : String)
    (carried : collection ∈ merged.replicatorCollections)
    (added : collection ∉ base.replicatorCollections) :
    collection ∈ named ∧ facts.authorHoldsGrant = true ∧ facts.bindingCurrent = true ∧
      facts.collectionsAdmitted = true := by
  by_cases hc : contributes facts = true
  · have hsub := route_carries_only_base_and_named localDid peerDid base facts named merged hm
    have hin := hsub carried
    simp only [Finset.mem_union, added, false_or] at hin
    obtain ⟨_, hb, _, hg, _, _, ha⟩ := (contributes_iff facts).mp hc
    exact ⟨hin, hg, hb, ha⟩
  · have hf : contributes facts = false := by simpa using hc
    simp only [route, overlayLayer, hf, Bool.false_eq_true, ↓reduceIte, mergeLayered,
      Option.bind_some, materializeBase] at hm
    split_ifs at hm <;> simp [mergeMaterialized] at hm <;> subst hm <;>
      exact absurd carried added

/-- An overlay that contributes nothing leaves exactly the enrollment's base
route: its collections stop at the next sweep. -/
theorem inactive_overlay_leaves_base (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (inactive : contributes facts = false) :
    route localDid peerDid base facts named = mergeLayered localDid peerDid (some base) none := by
  simp [route, overlayLayer, inactive]

/-- Withdrawing the author's P2P mutation grant stops replication of the named
collections at the next sweep, without deleting the overlay. -/
theorem withdrawn_grant_stops_replication (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (withdrawn : facts.authorHoldsGrant = false) :
    route localDid peerDid base facts named = mergeLayered localDid peerDid (some base) none :=
  inactive_overlay_leaves_base localDid peerDid base facts named
    (withdrawn_grant_contributes_nothing facts withdrawn)

/-- Disabling or deleting the author stops replication of the named collections. -/
theorem disabled_author_stops_replication (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (disabled : facts.authorLiveAgent = false) :
    route localDid peerDid base facts named = mergeLayered localDid peerDid (some base) none :=
  inactive_overlay_leaves_base localDid peerDid base facts named
    (disabled_author_contributes_nothing facts disabled)

/-- Ending the enrollment the overlay was applied under, by revocation or by
lapse, stops replication of the named collections for every later history and
every later generation of that member, including a re-approval of the same
peer: only a newly applied overlay can name them again. -/
theorem ended_enrollment_stops_replication (localDid peerDid : String) (base : Layer)
    (facts : Facts) (named : Finset String) (history later : List Revision) (b : Binding)
    (r : Revision) (ended : EndedBy history b r) (grows : history ⊆ later) (c : Current)
    (reaches : r.sequence ≤ c.sequence) (judged : facts.bindingCurrent = bindingCurrent later b c) :
    route localDid peerDid base facts named = mergeLayered localDid peerDid (some base) none := by
  apply inactive_overlay_leaves_base
  apply ended_binding_contributes_nothing
  rw [judged]
  exact ended_binding_never_revives history b r ended later grows c reaches

/-- Without an overlay (none was applied, or the author or the Node's
administration deleted it) the peer receives only its base route. -/
theorem no_overlay_is_base_only (localDid peerDid : String) (base : Layer)
    (distinct : peerDid ≠ localDid) :
    mergeLayered localDid peerDid (some base) none = some base := by
  simp [mergeLayered, materializeBase, distinct, mergeMaterialized]

end PairingReconcile.ApplicationRoute

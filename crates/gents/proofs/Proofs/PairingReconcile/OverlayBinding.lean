import Lean.Data.Json
import Mathlib.Data.List.Basic

/-!
When an application-collection pairing overlay may contribute collections to a
route. The overlay is the opt-in an Agent wrote for one enrolled peer; the
enrollment it was applied under and its author's current authority decide
whether it still counts.

Rust owner: `crates/gents/src/runtime/agent/p2p_reconcile/overlay_binding.rs`.
-/

namespace PairingReconcile.OverlayBinding

open Lean

/-- One signed authorization revision for the bound member, in its enrollment
scope (network, member DID, member peer). `active` is an approval; `false` is a
revocation tombstone. Times are instants on one clock.

External premise (`NetworkAuthorizationRevision` SDL and the enrollment owner):
revisions are append-only, revocation is a tombstone, history is never deleted,
the unique maximal sequence is authoritative and each new approval takes the
next sequence. Unverified rows can only make `bindingCurrent` false, because
every revision at a sequence must pass. -/
structure Revision where
  sequence : Nat
  active : Bool
  issuedAt : Nat
  expiresAt : Nat
  deriving DecidableEq, Repr

/-- The enrollment an overlay was applied under, recorded on the overlay by
`pairings_apply`: the peer's member DID and the authorization sequence then
current. Applying again rebinds it to the then-current sequence. -/
structure Binding where
  member : String
  sequence : Nat
  deriving DecidableEq, Repr

/-- The peer's current fresh authorization, as the enrollment authority reports
it to the reconciler. -/
structure Current where
  member : String
  sequence : Nat
  deriving DecidableEq, Repr

def inWindow (b : Binding) (c : Current) (r : Revision) : Bool :=
  decide (b.sequence ≤ r.sequence ∧ r.sequence ≤ c.sequence)

/-- A later revision continues its predecessor only when it was issued before
every revision at the previous sequence expired. -/
def continues (history : List Revision) (r : Revision) : Bool :=
  history.all fun p => !(p.sequence + 1 == r.sequence) || decide (r.issuedAt < p.expiresAt)

/-- Decision (security review, 2026-11-10): a pairing must not survive its
enrollment. The bound enrollment is still current exactly when the same member
has been authorized without interruption from the bound sequence to the current
one: every sequence in between exists, every revision there is an approval, and
each later approval was issued before its predecessor expired. A lease renewal
is such an approval, so it keeps the binding; a revocation, an expiry followed by
a new approval, or a different member ends it. -/
def bindingCurrent (history : List Revision) (b : Binding) (c : Current) : Bool :=
  b.member == c.member && decide (b.sequence ≤ c.sequence) &&
    (List.range' b.sequence (c.sequence + 1 - b.sequence)).all
      (fun k => history.any (·.sequence == k)) &&
    history.all fun r =>
      !inWindow b c r || (r.active && (r.sequence == b.sequence || continues history r))

/-- What the reconciler reads for one overlay before it may contribute.

Decision (security review, 2026-11-10): an overlay contributes only while its
author is a currently configured, enabled Agent of this node whose configured
tools currently carry the P2P mutation grant (the `p2p-pairing` source in
`Identity.Configuration.toolOperationSources`). Withdrawing the grant, disabling
or deleting the Agent stops the route at the next sweep; a revoked grant that is
granted again resumes it only while the binding is still current. The Node's
user owner can always delete any overlay
(`Identity.Configuration.pairing_overlay_delete_is_owner_scoped`). -/
structure Facts where
  enrollmentFresh : Bool
  bindingCurrent : Bool
  authorLiveAgent : Bool
  authorHoldsGrant : Bool
  collectionsAdmitted : Bool
  deriving DecidableEq, Repr

inductive Inactive where
  | enrollmentNotFresh
  | bindingEnded
  | authorNotAgent
  | authorLacksGrant
  | collectionsRefused
  deriving DecidableEq, Repr

def Inactive.name : Inactive → String
  | .enrollmentNotFresh => "enrollment_not_fresh"
  | .bindingEnded => "binding_ended"
  | .authorNotAgent => "author_not_agent"
  | .authorLacksGrant => "author_lacks_grant"
  | .collectionsRefused => "collections_refused"

/-- The first failed precondition, in a fixed order, or `none` when the overlay
contributes. -/
def decideOverlay (f : Facts) : Option Inactive :=
  if !f.enrollmentFresh then some .enrollmentNotFresh
  else if !f.bindingCurrent then some .bindingEnded
  else if !f.authorLiveAgent then some .authorNotAgent
  else if !f.authorHoldsGrant then some .authorLacksGrant
  else if !f.collectionsAdmitted then some .collectionsRefused
  else none

def contributes (f : Facts) : Bool := (decideOverlay f).isNone

theorem contributes_iff (f : Facts) :
    contributes f = true ↔
      f.enrollmentFresh ∧ f.bindingCurrent ∧ f.authorLiveAgent ∧ f.authorHoldsGrant ∧
        f.collectionsAdmitted := by
  cases f with
  | mk a b c d e => cases a <;> cases b <;> cases c <;> cases d <;> cases e <;> simp [contributes, decideOverlay]

theorem withdrawn_grant_contributes_nothing (f : Facts) (h : f.authorHoldsGrant = false) :
    contributes f = false := by
  cases f with
  | mk a b c d e => dsimp only at h; subst h; cases a <;> cases b <;> cases c <;> cases e <;> simp [contributes, decideOverlay]

theorem disabled_author_contributes_nothing (f : Facts) (h : f.authorLiveAgent = false) :
    contributes f = false := by
  cases f with
  | mk a b c d e => dsimp only at h; subst h; cases a <;> cases b <;> cases d <;> cases e <;> simp [contributes, decideOverlay]

theorem ended_binding_contributes_nothing (f : Facts) (h : f.bindingCurrent = false) :
    contributes f = false := by
  cases f with
  | mk a b c d e => dsimp only at h; subst h; cases a <;> cases c <;> cases d <;> cases e <;> simp [contributes, decideOverlay]

/-- A revocation of the member anywhere from the bound sequence up to the
current one ends the binding. -/
theorem revocation_in_window_ends_binding (history : List Revision) (b : Binding) (c : Current)
    (r : Revision) (mem : r ∈ history) (window : inWindow b c r = true) (revoked : r.active = false) :
    bindingCurrent history b c = false := by
  apply Bool.eq_false_iff.mpr
  intro h
  simp only [bindingCurrent, Bool.and_eq_true, List.all_eq_true] at h
  have hr := h.2 r mem
  simp [window, revoked] at hr

/-- An approval issued at or after its predecessor's expiry is a new enrollment,
not a renewal, and ends the binding. -/
theorem lapse_in_window_ends_binding (history : List Revision) (b : Binding) (c : Current)
    (p r : Revision) (hp : p ∈ history) (hr : r ∈ history) (window : inWindow b c r = true)
    (later : r.sequence ≠ b.sequence) (pred : p.sequence + 1 = r.sequence)
    (lapsed : p.expiresAt ≤ r.issuedAt) :
    bindingCurrent history b c = false := by
  apply Bool.eq_false_iff.mpr
  intro h
  simp only [bindingCurrent, Bool.and_eq_true, List.all_eq_true] at h
  have hr' := h.2 r hr
  simp only [window, Bool.not_true, Bool.false_or, Bool.and_eq_true, Bool.or_eq_true,
    beq_iff_eq, later, false_or, continues, List.all_eq_true] at hr'
  have hp' := hr'.2 p hp
  simp [pred, Nat.not_lt.mpr lapsed] at hp'

/-- History is append-only, so once an enrollment ended no later history
revives an overlay bound to it: any extension still holds the revocation. -/
theorem ended_binding_never_revives (history later : List Revision) (b : Binding) (c : Current)
    (r : Revision) (mem : r ∈ history) (grows : history ⊆ later)
    (window : inWindow b c r = true) (revoked : r.active = false) :
    bindingCurrent later b c = false :=
  revocation_in_window_ends_binding later b c r (grows mem) window revoked

theorem other_member_ends_binding (history : List Revision) (b : Binding) (c : Current)
    (h : b.member ≠ c.member) : bindingCurrent history b c = false := by
  simp [bindingCurrent, h]

def approval (sequence issuedAt expiresAt : Nat) : Revision :=
  { sequence, active := true, issuedAt, expiresAt }

def revocation (sequence issuedAt : Nat) : Revision :=
  { sequence, active := false, issuedAt, expiresAt := issuedAt }

theorem renewal_keeps_binding :
    bindingCurrent [approval 1 0 100, approval 2 90 200] ⟨"member", 1⟩ ⟨"member", 2⟩ = true := by
  decide

theorem reapproval_after_revoke_ends_binding :
    bindingCurrent [approval 1 0 100, revocation 2 50, approval 3 60 200]
      ⟨"member", 1⟩ ⟨"member", 3⟩ = false := by
  decide

theorem reapproval_after_expiry_ends_binding :
    bindingCurrent [approval 1 0 100, approval 2 150 300] ⟨"member", 1⟩ ⟨"member", 2⟩ = false := by
  decide

theorem rebinding_after_reapproval_is_current :
    bindingCurrent [approval 1 0 100, revocation 2 50, approval 3 60 200]
      ⟨"member", 3⟩ ⟨"member", 3⟩ = true := by
  decide

/-! Generated cases for the Rust owner. -/

def revisionJson (r : Revision) : Json :=
  Json.mkObj [("sequence", toJson r.sequence), ("active", toJson r.active),
    ("issued_at", toJson r.issuedAt), ("expires_at", toJson r.expiresAt)]

def bindingCase (name : String) (history : List Revision) (b : Binding) (c : Current) : Json :=
  Json.mkObj [("name", toJson name), ("history", Json.arr (history.map revisionJson).toArray),
    ("binding_member", toJson b.member), ("binding_sequence", toJson b.sequence),
    ("current_member", toJson c.member), ("current_sequence", toJson c.sequence),
    ("current", toJson (bindingCurrent history b c))]

def bindingCases : List Json :=
  [bindingCase "same-generation" [approval 1 0 100] ⟨"m", 1⟩ ⟨"m", 1⟩,
   bindingCase "renewed" [approval 1 0 100, approval 2 90 200] ⟨"m", 1⟩ ⟨"m", 2⟩,
   bindingCase "renewed-twice" [approval 1 0 100, approval 2 90 200, approval 3 190 300]
     ⟨"m", 1⟩ ⟨"m", 3⟩,
   bindingCase "revoked-then-reapproved" [approval 1 0 100, revocation 2 50, approval 3 60 200]
     ⟨"m", 1⟩ ⟨"m", 3⟩,
   bindingCase "expired-then-reapproved" [approval 1 0 100, approval 2 150 300]
     ⟨"m", 1⟩ ⟨"m", 2⟩,
   bindingCase "rebound-after-reapproval" [approval 1 0 100, revocation 2 50, approval 3 60 200]
     ⟨"m", 3⟩ ⟨"m", 3⟩,
   bindingCase "other-member" [approval 1 0 100] ⟨"m", 1⟩ ⟨"n", 1⟩,
   bindingCase "bound-ahead" [approval 1 0 100] ⟨"m", 2⟩ ⟨"m", 1⟩,
   bindingCase "missing-sequence" [approval 1 0 100, approval 3 90 200] ⟨"m", 1⟩ ⟨"m", 3⟩,
   bindingCase "bound-revocation" [approval 1 0 100, revocation 2 50] ⟨"m", 2⟩ ⟨"m", 2⟩,
   bindingCase "conflicting-revision" [approval 1 0 100, approval 2 90 200, revocation 2 95]
     ⟨"m", 1⟩ ⟨"m", 2⟩]

def factsCase (f : Facts) : Json :=
  Json.mkObj [("enrollment_fresh", toJson f.enrollmentFresh),
    ("binding_current", toJson f.bindingCurrent), ("author_live_agent", toJson f.authorLiveAgent),
    ("author_holds_grant", toJson f.authorHoldsGrant),
    ("collections_admitted", toJson f.collectionsAdmitted),
    ("inactive", match decideOverlay f with | some reason => toJson reason.name | none => Json.null)]

def factsCases : List Json :=
  let bools := [false, true]
  bools.flatMap fun a => bools.flatMap fun b => bools.flatMap fun c => bools.flatMap fun d =>
    bools.map fun e => factsCase ⟨a, b, c, d, e⟩

def bindingCasesJson : String := (Json.arr bindingCases.toArray).compress

def factsCasesJson : String := (Json.arr factsCases.toArray).compress

end PairingReconcile.OverlayBinding

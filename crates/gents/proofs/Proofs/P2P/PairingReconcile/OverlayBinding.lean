import Lean.Data.Json
import Mathlib.Data.List.Basic

/-!
When an application-collection pairing overlay may contribute collections to a
route. The overlay is the opt-in an Agent wrote for one enrolled peer; the
enrollment it was applied under and its author's current authority decide
whether it still counts.

Rust owner: `crates/gents-p2p/src/p2p_reconcile/overlay_binding.rs`.
-/

namespace PairingReconcile.OverlayBinding

open Lean

/-- One signed authorization revision for the bound member, in its enrollment
scope (network and member DID). `active` is an approval; `false` is a
revocation tombstone. Times are instants on this node's clock.

Premise (append-only history): a revision, once written, is never changed or
removed. In code this rests on the `NetworkAuthorizationRevision` SDL marking
every field `@immutable`, on no Gents owner deleting revisions (the renewal
cleanup prunes only `EnrollmentRenewalOffer`), and on revocation being a
tombstone revision. Residual gap: the `enrollment_records` resource's delete
expression is empty, so the node signer could still delete a revision natively;
enrollment ACP is unchanged by application pairing. Unverified rows can only make
`bindingCurrent` false, because every revision at a sequence must pass. -/
structure Revision where
  sequence : Nat
  active : Bool
  issuedAt : Nat
  expiresAt : Nat
  deriving DecidableEq, Repr

/-- The enrollment an overlay was applied under, recorded on the overlay by
`pairings_apply`: the network, the peer's member DID and the authorization
sequence then current. Applying again rebinds it to the then-current
generation. -/
structure Binding where
  network : String
  member : String
  sequence : Nat
  deriving DecidableEq, Repr

/-- The peer's current fresh authorization, as the enrollment authority reports
it to the reconciler. -/
structure Current where
  network : String
  member : String
  sequence : Nat
  deriving DecidableEq, Repr

def inWindow (b : Binding) (c : Current) (r : Revision) : Bool :=
  decide (b.sequence ≤ r.sequence ∧ r.sequence ≤ c.sequence)

/-- A later revision continues its predecessor only when it was issued before
every revision at the previous sequence expired.

Premise (clock): renewal and a new approval after expiry are told apart only by
`issuedAt < expiresAt` on this node's clock. A clock set backwards when a
re-approval is issued could make a re-approval after expiry look like a
renewal. -/
def continues (history : List Revision) (r : Revision) : Bool :=
  history.all fun p => !(p.sequence + 1 == r.sequence) || decide (r.issuedAt < p.expiresAt)

/-- Decision (security review, 2026-11-10): a pairing must not survive its
enrollment. The bound enrollment is still current exactly when the network and
member are the same and the member has been authorized without interruption from
the bound sequence to the current one: every sequence in between exists, every
revision there is an approval, and each later approval was issued before its
predecessor expired. A lease renewal is such an approval, so it keeps the
binding; a revocation, an expiry followed by a new approval, a different member
or a re-created network ends it.

Honest limit (second review): any actor holding P2P mutation can both approve an
enrollment and apply a pairing. The binding guarantees that application data
flows after an enrollment ends only if someone writes the pairing again; it does
not guarantee a fresh decision by the User. -/
def bindingCurrent (history : List Revision) (b : Binding) (c : Current) : Bool :=
  b.network == c.network && b.member == c.member && decide (b.sequence ≤ c.sequence) &&
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
user owner and its administrators can always delete any overlay
(`Identity.ApplicationPairingAccess.node_administration_reads_and_deletes`).

Decision (second review, 2026-11-10; settled 2026-11-12): the overlay's content
must have been written only by its author (`contentOwnerWritten`: every native
commit of the document is signed by the author). The overlay has its own policy
resource, `application_pairings`, whose update names no relation, so document
ACP admits an update only from the author
(`Identity.ApplicationPairingAccess.only_the_author_updates`); a fresh store
binds it there, so no migration is involved. A write that bypasses document ACP
(a NAC owner's DAC bypass on the HTTP path) is still possible natively, so the
reconciler keeps refusing any content another signer wrote: without it, such a
writer who does not hold the grant could re-bind or widen an Agent's pairing and
have it run under the Agent's grant.

An overlay naming no known template, or a template other than the application
one, is inactive (`templateValid`) rather than an error that stalls the peer. -/
structure Facts where
  enrollmentFresh : Bool
  bindingCurrent : Bool
  authorLiveAgent : Bool
  authorHoldsGrant : Bool
  contentOwnerWritten : Bool
  templateValid : Bool
  collectionsAdmitted : Bool
  deriving DecidableEq, Repr

inductive Inactive where
  | enrollmentNotFresh
  | bindingEnded
  | authorNotAgent
  | authorLacksGrant
  | foreignWrite
  | invalidTemplate
  | collectionsRefused
  deriving DecidableEq, Repr

def Inactive.name : Inactive → String
  | .enrollmentNotFresh => "enrollment_not_fresh"
  | .bindingEnded => "binding_ended"
  | .authorNotAgent => "author_not_agent"
  | .authorLacksGrant => "author_lacks_grant"
  | .foreignWrite => "foreign_write"
  | .invalidTemplate => "invalid_template"
  | .collectionsRefused => "collections_refused"

/-- The first failed precondition, in a fixed order, or `none` when the overlay
contributes. -/
def decideOverlay (f : Facts) : Option Inactive :=
  if !f.enrollmentFresh then some .enrollmentNotFresh
  else if !f.bindingCurrent then some .bindingEnded
  else if !f.authorLiveAgent then some .authorNotAgent
  else if !f.authorHoldsGrant then some .authorLacksGrant
  else if !f.contentOwnerWritten then some .foreignWrite
  else if !f.templateValid then some .invalidTemplate
  else if !f.collectionsAdmitted then some .collectionsRefused
  else none

def contributes (f : Facts) : Bool := (decideOverlay f).isNone

theorem contributes_iff (f : Facts) :
    contributes f = true ↔
      f.enrollmentFresh ∧ f.bindingCurrent ∧ f.authorLiveAgent ∧ f.authorHoldsGrant ∧
        f.contentOwnerWritten ∧ f.templateValid ∧ f.collectionsAdmitted := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

theorem withdrawn_grant_contributes_nothing (f : Facts) (h : f.authorHoldsGrant = false) :
    contributes f = false := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

theorem disabled_author_contributes_nothing (f : Facts) (h : f.authorLiveAgent = false) :
    contributes f = false := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

theorem ended_binding_contributes_nothing (f : Facts) (h : f.bindingCurrent = false) :
    contributes f = false := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

/-- A write by anyone other than the author voids the overlay, whatever ACP
admitted. -/
theorem foreign_write_contributes_nothing (f : Facts) (h : f.contentOwnerWritten = false) :
    contributes f = false := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

theorem invalid_template_contributes_nothing (f : Facts) (h : f.templateValid = false) :
    contributes f = false := by
  simp only [contributes, decideOverlay]
  split_ifs <;> simp_all

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

/-- How an enrollment bound at `b` can end: a revocation at or after the bound
sequence, or a later approval issued at or after its predecessor expired. -/
def EndedBy (history : List Revision) (b : Binding) (r : Revision) : Prop :=
  r ∈ history ∧ b.sequence ≤ r.sequence ∧
    (r.active = false ∨
      (r.sequence ≠ b.sequence ∧
        ∃ p ∈ history, p.sequence + 1 = r.sequence ∧ p.expiresAt ≤ r.issuedAt))

/-- Once an enrollment ended, by revocation or by lapse, no later history and no
later current generation revive an overlay bound to it: history only grows
(premise on `Revision`), so the ending revision stays in every later window. -/
theorem ended_binding_never_revives (history : List Revision) (b : Binding) (r : Revision)
    (ended : EndedBy history b r) (later : List Revision) (grows : history ⊆ later)
    (c : Current) (reaches : r.sequence ≤ c.sequence) :
    bindingCurrent later b c = false := by
  obtain ⟨mem, lower, how⟩ := ended
  have window : inWindow b c r = true := by simp [inWindow, lower, reaches]
  rcases how with revoked | ⟨later_seq, p, hp, pred, lapsed⟩
  · exact revocation_in_window_ends_binding later b c r (grows mem) window revoked
  · exact lapse_in_window_ends_binding later b c p r (grows hp) (grows mem) window later_seq pred
      lapsed

theorem other_member_ends_binding (history : List Revision) (b : Binding) (c : Current)
    (h : b.member ≠ c.member) : bindingCurrent history b c = false := by
  simp [bindingCurrent, h]

theorem other_network_ends_binding (history : List Revision) (b : Binding) (c : Current)
    (h : b.network ≠ c.network) : bindingCurrent history b c = false := by
  simp [bindingCurrent, h]

def approval (sequence issuedAt expiresAt : Nat) : Revision :=
  { sequence, active := true, issuedAt, expiresAt }

def revocation (sequence issuedAt : Nat) : Revision :=
  { sequence, active := false, issuedAt, expiresAt := issuedAt }

theorem renewal_keeps_binding :
    bindingCurrent [approval 1 0 100, approval 2 90 200] ⟨"net", "member", 1⟩ ⟨"net", "member", 2⟩ = true := by
  decide

theorem reapproval_after_revoke_ends_binding :
    bindingCurrent [approval 1 0 100, revocation 2 50, approval 3 60 200]
      ⟨"net", "member", 1⟩ ⟨"net", "member", 3⟩ = false := by
  decide

theorem reapproval_after_expiry_ends_binding :
    bindingCurrent [approval 1 0 100, approval 2 150 300] ⟨"net", "member", 1⟩ ⟨"net", "member", 2⟩ = false := by
  decide

theorem rebinding_after_reapproval_is_current :
    bindingCurrent [approval 1 0 100, revocation 2 50, approval 3 60 200]
      ⟨"net", "member", 3⟩ ⟨"net", "member", 3⟩ = true := by
  decide

end PairingReconcile.OverlayBinding

import Proofs.Store.Identity.GrantSets
import Proofs.Store.Identity.Household

/-!
The authority an explicit enrollment grant derives for one verified client
enrollment, and what revoking that enrollment withdraws.

Rust owner: `crates/gents-store/src/identity/enrollment_grant.rs`
(`EnrollmentGrantTuple`, `plan_grant`, `plan_withdrawal`, `approve_enrollment`),
written by every enrollment approval and by `gents p2p enrollment grant`;
withdrawn directly by an approver's decision (`withdraw_as`), and retired with
its administration withdrawal owed by `GraphqlEnrollmentStore` in the
transaction that publishes the denial or revocation (`oweRevoke`), which the
owner's client then issues (`settle`, `ClientAccessOwner::settle_withdrawals`).
-/
namespace Identity.EnrollmentGrant
open Identity.GrantSets

/-- One native authority tuple the grant derives for its member: the Node
document's `administrator` relation, or one `operatorUser` node permission. -/
inductive Tuple where
  | nodeAdministrator
  | permission (name : String)
  deriving DecidableEq, Repr

def Tuple.name : Tuple → String
  | .nodeAdministrator => "node-administrator"
  | .permission p => p

/-- Exactly the authority one enrollment grant derives. -/
def derived : List Tuple :=
  .nodeAdministrator :: GrantSet.operatorUser.permissions.map Tuple.permission

/-- A native tuple held by a subject. -/
structure Held where
  subject : String
  tuple : Tuple
  deriving DecidableEq, Repr

/-- The durable receipt one enrollment grant writes before it issues any
tuple. Native DAC relations and direct NAC permissions carry no provenance:
an identical grant from another source is the same tuple. The receipt is
therefore the only record of which tuples the enrollment accounts for. It is
written first, so a grant interrupted after it leaves no unrecorded authority,
and revocation withdraws it in the revision's transaction.

`owed` marks a withdrawn receipt whose member's Node `administrator` relation
was due to be withdrawn by an actor that may not remove it (`oweRevoke`): the
withdrawal waits for the Node owner or an administrator (`settle`). -/
structure Receipt where
  request : String
  member : String
  tuples : List Tuple
  withdrawn : Bool := false
  owed : Bool := false
  restore : Bool := false
  deriving DecidableEq, Repr

structure State where
  held : List Held
  receipts : List Receipt
  deriving DecidableEq, Repr

def holds (s : State) (member : String) (t : Tuple) : Bool :=
  s.held.contains ⟨member, t⟩

def accounts (r : Receipt) (member : String) (t : Tuple) : Bool :=
  !r.withdrawn && r.member == member && r.tuples.contains t

/-- Some live enrollment receipt accounts for this tuple. -/
def accounted (s : State) (member : String) (t : Tuple) : Bool :=
  s.receipts.any (accounts · member t)

/-- Some retired receipt still owes the withdrawal of `member`'s Node
`administrator` relation. Only that relation is ever owed: node permissions
are withdrawn by the runtime as the NAC owner (`NodeProvisioning.nacOwner`). -/
def owedFor (s : State) (member : String) : Bool :=
  s.receipts.any (fun r => r.owed && r.member == member)

/-- Held by another, independent grant: present natively, accounted for by
no live enrollment receipt and not an owed withdrawal. The owner's own
authority (Node ownership and the native `admin` relation) is never a derived
tuple and is independent. -/
def independent (s : State) (member : String) (t : Tuple) : Bool :=
  holds s member t && !accounted s member t && !(t == .nodeAdministrator && owedFor s member)

/-- A grant records every derived tuple except those held independently. A
tuple another live enrollment already accounts for is recorded again, so it
stays held until the last enrollment accounting for it is revoked. -/
def record (s : State) (member : String) : List Tuple :=
  derived.filter (fun t => !independent s member t)

def liveReceipt (s : State) (request : String) : Option Receipt :=
  s.receipts.find? (fun r => r.request == request && !r.withdrawn)

/-- The tuples of a live receipt a repeated grant for `member` issues again:
the derived ones it recorded that `member` does not hold. A receipt only ever
names derived tuples (`EnrollmentGrantTuple::parse`). -/
def reissued (s : State) (r : Receipt) (member : String) : List Tuple :=
  r.tuples.filter (fun t => derived.contains t && !holds s member t)

/-- Issue again every derived tuple a live receipt recorded that `member` does
not hold. -/
def reissue (s : State) (r : Receipt) (member : String) : State :=
  { s with held := s.held ++ (reissued s r member).map (⟨member, ·⟩) }

/-- `member`'s owed withdrawal is closed once issued. -/
def clearOwed (rs : List Receipt) (member : String) : List Receipt :=
  rs.map (fun r => if r.member == member then { r with owed := false } else r)

/-- `member`'s owed withdrawal is taken over by someone who wants the member
to hold the relation (a new enrollment, a manager's deliberate grant): the
owed receipts stop owing the removal and owe the relation's restoration
instead, in the same write. A settlement pass whose removal was already in
flight can still remove the relation after the takeover; the restoration
owed keeps that from being silent (`restoreBy`, `tla/SettlementApproval.tla`).

Decision (owner, gents#3979 review): the repair of a removal that overtook an
approval stays pending durably until a manager's client confirms the relation
is held. -/
def takeOver (rs : List Receipt) (member : String) : List Receipt :=
  rs.map (fun r => if r.member == member && r.owed then { r with owed := false, restore := true }
    else r)

/-- One receipt per request: a repeated grant of a request with a live receipt
reissues exactly what that receipt recorded, and a withdrawn request grants
nothing. A fresh grant to a member whose administration withdrawal is owed
records that relation (it is not independent) and takes the owed withdrawal
over in the same receipt write: the member is administrator again by this
enrollment, and revoking it withdraws the relation. -/
def grant (s : State) (request member : String) : State :=
  if s.receipts.any (·.request == request) then
    match liveReceipt s request with
    | some r => reissue s r member
    | none => s
  else
    { held := s.held ++ ((record s member).filter (fun t => !holds s member t)).map (⟨member, ·⟩)
      receipts := ⟨request, member, record s member, false, false, false⟩ :: takeOver s.receipts member }

/-- A grant from any other source. Native tuples are a set: granting a held
tuple changes nothing and leaves no provenance. External premise: the pinned
DefraDB DAC relationship store and NAC actor relationships. -/
def grantIndependent (s : State) (a : Held) : State :=
  if s.held.contains a then s else { s with held := a :: s.held }

def withdraw (rs : List Receipt) (request : String) : List Receipt :=
  rs.map (fun r => if r.request == request then { r with withdrawn := true } else r)

/-- Removed by revoking `r`: a tuple `r` recorded for its member that no other
live receipt still accounts for. -/
def removedBy (s : State) (r : Receipt) (a : Held) : Bool :=
  a.subject == r.member && r.tuples.contains a.tuple &&
    !(withdraw s.receipts r.request).any (accounts · r.member a.tuple)

/-- Revocation withdraws the request's live receipt and exactly the tuples it
alone accounted for. Without a live receipt it changes nothing. -/
def revoke (s : State) (request : String) : State :=
  match liveReceipt s request with
  | none => s
  | some r =>
      { held := s.held.filter (fun a => !removedBy s r a)
        receipts := withdraw s.receipts request }

/-- The tuples a grant of `request` issues, in order: those its receipt
records that the member does not hold yet. A fresh request records them
first. -/
def toIssue (s : State) (request member : String) : List Tuple :=
  match liveReceipt s request with
  | some r => reissued s r member
  | none => (record s member).filter (fun t => !holds s member t)

/-- A grant whose issuing failed after its first `issued` tuples. The receipt
is written before any tuple, and tuples are issued one at a time. -/
def grantInterrupted (s : State) (request member : String) (issued : Nat) : State :=
  match liveReceipt s request with
  | some _ => { s with held := s.held ++ ((toIssue s request member).take issued).map (⟨member, ·⟩) }
  | none =>
      if s.receipts.any (·.request == request) then s
      else
        { held := s.held ++ ((toIssue s request member).take issued).map (⟨member, ·⟩)
          receipts := ⟨request, member, record s member, false, false, false⟩ :: takeOver s.receipts member }

/-- Decision (gents#3930 review): an approval whose tuples could not all be
issued removes every tuple its receipt alone accounts for, as revoking would,
but keeps the receipt live and the request pending. A failed approval leaves
no authority it granted; a retried approval reissues everything, and denying
or revoking the request still withdraws its receipt. -/
def abandon (s : State) (request : String) : State :=
  match liveReceipt s request with
  | none => s
  | some r => { s with held := s.held.filter (fun a => !removedBy s r a) }

theorem liveReceipt_spec {s : State} {request : String} {r : Receipt}
    (h : liveReceipt s request = some r) :
    r ∈ s.receipts ∧ r.request = request ∧ r.withdrawn = false := by
  unfold liveReceipt at h
  have mem := List.mem_of_find?_eq_some h
  have p := List.find?_some h
  simp at p
  exact ⟨mem, p.1, p.2⟩

/-- After revocation the member holds none of the authority the enrollment
alone derived: each recorded tuple is gone unless another live enrollment
still accounts for it. -/
theorem revoke_withdraws_derived (s : State) (request : String) (r : Receipt)
    (live : liveReceipt s request = some r) (t : Tuple) (recorded : r.tuples.contains t = true)
    (alone : accounted (revoke s request) r.member t = false) :
    holds (revoke s request) r.member t = false := by
  have ⟨_, req, _⟩ := liveReceipt_spec live
  simp only [revoke, live] at alone ⊢
  simp only [accounted] at alone
  simp only [holds, removedBy, req]
  rw [Bool.eq_false_iff]
  intro hc
  simp only [List.contains_iff_mem, List.mem_filter] at hc
  have hr := hc.2
  simp only [recorded, Bool.true_and, beq_self_eq_true, alone, Bool.not_false,
    Bool.not_true] at hr
  exact absurd hr (by decide)

/-- Revocation never removes an independent grant. -/
theorem revoke_preserves_independent (s : State) (request subject : String) (t : Tuple)
    (indep : independent s subject t = true) :
    (revoke s request).held.contains ⟨subject, t⟩ = true := by
  have heldA : s.held.contains ⟨subject, t⟩ = true := by
    simp only [independent, holds, Bool.and_eq_true] at indep
    exact indep.1.1
  have notAcc : accounted s subject t = false := by
    simp only [independent, Bool.and_eq_true, Bool.not_eq_true'] at indep
    exact indep.1.2
  unfold revoke
  split
  · exact heldA
  · rename_i r live
    have ⟨mem, _, notWithdrawn⟩ := liveReceipt_spec live
    have notRemoved : removedBy s r ⟨subject, t⟩ = false := by
      rw [Bool.eq_false_iff]
      intro h
      simp only [removedBy, Bool.and_eq_true, beq_iff_eq] at h
      have acc : accounts r subject t = true := by
        simp only [accounts, notWithdrawn, h.1.1, h.1.2, beq_self_eq_true, Bool.not_false,
          Bool.and_self]
      have : accounted s subject t = true := List.any_eq_true.mpr ⟨r, mem, acc⟩
      rw [this] at notAcc
      exact absurd notAcc (by decide)
    rw [List.contains_iff_mem] at heldA ⊢
    exact List.mem_filter.mpr ⟨heldA, by simp [notRemoved]⟩

/-- Revocation removes nothing but tuples the revoked receipt recorded for its
member: never another subject's authority, never an unrecorded tuple. -/
theorem revoke_removes_only_recorded (s : State) (request : String) (a : Held)
    (before : a ∈ s.held) (after : a ∉ (revoke s request).held) :
    ∃ r, liveReceipt s request = some r ∧ a.subject = r.member ∧ a.tuple ∈ r.tuples := by
  unfold revoke at after
  split at after
  · exact absurd before after
  · rename_i r live
    refine ⟨r, live, ?_⟩
    have removed : removedBy s r a = true := by
      by_contra h
      apply after
      exact List.mem_filter.mpr ⟨before, by simpa using h⟩
    simp only [removedBy, Bool.and_eq_true, beq_iff_eq] at removed
    exact ⟨removed.1.1, List.contains_iff_mem.mp removed.1.2⟩

theorem withdraw_has_no_live (rs : List Receipt) (request : String) :
    (withdraw rs request).find? (fun r => r.request == request && !r.withdrawn) = none := by
  rw [List.find?_eq_none]
  intro x hx
  simp only [withdraw, List.mem_map] at hx
  obtain ⟨y, _, rfl⟩ := hx
  by_cases h : y.request == request <;> simp [h]

theorem revoke_of_no_live (s : State) (request : String) (h : liveReceipt s request = none) :
    revoke s request = s := by
  simp [revoke, h]

/-- Revocation is idempotent. -/
theorem revoke_idempotent (s : State) (request : String) :
    revoke (revoke s request) request = revoke s request := by
  apply revoke_of_no_live
  cases live : liveReceipt s request with
  | none => rw [revoke_of_no_live s request live]; exact live
  | some r =>
      simp only [revoke, live]
      exact withdraw_has_no_live s.receipts request

/-- An abandoned approval leaves its member none of the authority its receipt
alone accounted for: the Node administrator relation and every permission
another live receipt does not also account for are gone. -/
theorem abandon_withdraws_derived (s : State) (request : String) (r : Receipt)
    (live : liveReceipt s request = some r) (t : Tuple) (recorded : r.tuples.contains t = true)
    (alone : (withdraw s.receipts r.request).any (accounts · r.member t) = false) :
    holds (abandon s request) r.member t = false := by
  simp only [abandon, live, holds, removedBy]
  rw [Bool.eq_false_iff]
  intro hc
  simp only [List.contains_iff_mem, List.mem_filter] at hc
  have hr := hc.2
  simp only [recorded, Bool.true_and, beq_self_eq_true, alone, Bool.not_false,
    Bool.not_true] at hr
  exact absurd hr (by decide)

/-- Abandoning keeps every receipt as it was: the request stays pending and
its receipt live. -/
theorem abandon_keeps_receipts (s : State) (request : String) :
    (abandon s request).receipts = s.receipts := by
  unfold abandon
  split <;> rfl

/-- Abandoning never removes an independent grant. -/
theorem abandon_preserves_independent (s : State) (request subject : String) (t : Tuple)
    (indep : independent s subject t = true) :
    (abandon s request).held.contains ⟨subject, t⟩ = true := by
  have heldA : s.held.contains ⟨subject, t⟩ = true := by
    simp only [independent, holds, Bool.and_eq_true] at indep
    exact indep.1.1
  have notAcc : accounted s subject t = false := by
    simp only [independent, Bool.and_eq_true, Bool.not_eq_true'] at indep
    exact indep.1.2
  unfold abandon
  split
  · exact heldA
  · rename_i r live
    have ⟨mem, _, notWithdrawn⟩ := liveReceipt_spec live
    have notRemoved : removedBy s r ⟨subject, t⟩ = false := by
      rw [Bool.eq_false_iff]
      intro h
      simp only [removedBy, Bool.and_eq_true, beq_iff_eq] at h
      have acc : accounts r subject t = true := by
        simp only [accounts, notWithdrawn, h.1.1, h.1.2, beq_self_eq_true, Bool.not_false,
          Bool.and_self]
      have : accounted s subject t = true := List.any_eq_true.mpr ⟨r, mem, acc⟩
      rw [this] at notAcc
      exact absurd notAcc (by decide)
    rw [List.contains_iff_mem] at heldA ⊢
    exact List.mem_filter.mpr ⟨heldA, by simp [notRemoved]⟩

/-- Abandoning never removes a tuple another live approval accounts for. -/
theorem abandon_preserves_other_approvals (s : State) (request : String) (a : Held)
    (o : Receipt) (other : o ∈ s.receipts) (otherRequest : (o.request == request) = false)
    (owns : accounts o a.subject a.tuple = true) (before : a ∈ s.held) :
    a ∈ (abandon s request).held := by
  unfold abandon
  split
  · exact before
  · rename_i r live
    have ⟨_, req, _⟩ := liveReceipt_spec live
    refine List.mem_filter.mpr ⟨before, ?_⟩
    simp only [removedBy, Bool.not_and, Bool.not_not]
    by_cases hs : (a.subject == r.member) = true
    · have still : o ∈ withdraw s.receipts r.request := by
        simp only [withdraw, List.mem_map]
        refine ⟨o, other, ?_⟩
        rw [req, otherRequest]
        rfl
      have : (withdraw s.receipts r.request).any (accounts · r.member a.tuple) = true := by
        rw [beq_iff_eq] at hs
        rw [← hs]
        exact List.any_eq_true.mpr ⟨o, still, owns⟩
      simp [this]
    · simp [Bool.eq_false_iff.mpr hs]

/-- A grant never records a tuple the member held independently, so revoking
it can never withdraw that tuple. -/
theorem grant_records_no_independent (s : State) (member : String) (t : Tuple)
    (indep : independent s member t = true) : (record s member).contains t = false := by
  simp [record, indep]

/-- After a fresh grant the member holds every derived tuple. -/
theorem grant_derives_all (s : State) (request member : String)
    (fresh : s.receipts.any (·.request == request) = false) (t : Tuple) (d : t ∈ derived) :
    holds (grant s request member) member t = true := by
  rw [grant, if_neg (by simp [fresh])]
  unfold holds
  rw [List.contains_iff_mem, List.mem_append]
  by_cases h : holds s member t = true
  · exact Or.inl (List.contains_iff_mem.mp h)
  · refine Or.inr (List.mem_map.mpr ⟨t, List.mem_filter.mpr ⟨?_, by rw [Bool.not_eq_true']; exact Bool.eq_false_iff.mpr h⟩, rfl⟩)
    exact List.mem_filter.mpr ⟨d, by simp [independent, h]⟩

/-- Revoking a membership withdraws every live receipt its member holds,
whichever request issued it, and every tuple only those receipts accounted
for. A renewal is a new request with its own receipt, so withdrawing by
request alone would leave the earlier generation's receipt accounting for
the member's Node administration after revocation. Decision (maintainer,
2026-11-11, #3681): a revoked member keeps no authority any enrollment of
its membership derived. -/
def revokeMember (s : State) (member : String) : State :=
  { held := s.held.filter (fun a =>
      !(a.subject == member && s.receipts.any (accounts · member a.tuple)))
    receipts := s.receipts.map (fun r =>
      if r.member == member then { r with withdrawn := true } else r) }

/-- After a membership is revoked no live receipt accounts for its member. -/
theorem revokeMember_leaves_no_live_receipt (s : State) (member : String) (t : Tuple) :
    accounted (revokeMember s member) member t = false := by
  simp only [accounted, revokeMember, List.any_map, Bool.eq_false_iff]
  intro h
  obtain ⟨r, _, hr⟩ := List.any_eq_true.mp h
  by_cases hm : r.member = member
  · simp [Function.comp, accounts, hm] at hr
  · simp [Function.comp, accounts, hm] at hr

/-- After a membership is revoked its member holds no tuple any of its
enrollments accounted for. -/
theorem revokeMember_withdraws_every_generation (s : State) (member : String) (t : Tuple)
    (acc : accounted s member t = true) : holds (revokeMember s member) member t = false := by
  simp only [holds, revokeMember, Bool.eq_false_iff]
  intro h
  rw [List.contains_iff_mem, List.mem_filter] at h
  simp only [accounted] at acc
  simp [acc] at h

/-- Revoking a membership never removes an independent grant. -/
theorem revokeMember_preserves_independent (s : State) (member subject : String) (t : Tuple)
    (indep : independent s subject t = true) :
    (revokeMember s member).held.contains ⟨subject, t⟩ = true := by
  simp only [independent, holds, accounted, Bool.and_eq_true, Bool.not_eq_true'] at indep
  rw [List.contains_iff_mem] at indep ⊢
  simp only [revokeMember, List.mem_filter]
  refine ⟨indep.1.1, ?_⟩
  by_cases hs : subject = member
  · subst hs; simp [indep.1.2]
  · simp [hs]

/-! ### Operator decisions -/

/-- An operator's decision on a pending client enrollment. Only a client
enrollment derives authority; approving another profile admits its route
alone.

Decision: approving is admission. Decision (maintainer, 2026-11-11, #3681):
approving a client enrollment grants Node administration on every surface, to
be narrowed later. `gents p2p enrollment approve`, desktop pairing, the
desktop's Approve and the Agent `p2p` tool's `enrollment_approve` all run one
handler, `approve_enrollment`, which runs `grant` (receipt first, then the
derived tuples) before the approval commits, so an approval never stands
without its authority, a failed grant approves nothing, and a retried approval
finds the receipt and reissues it (`grant` is idempotent per request). An
approval whose tuples could not all be issued runs `abandon`, so a failed
approval leaves none of its authority behind. Denying
runs `revoke`, so a grant whose approval never committed is withdrawn and no
derived authority outlives a denial. Rust owners: `approve_enrollment` (grant,
then approve) and the runtime enrollment store's denial, which retires the
request's live receipt and owes its administration withdrawal
(`revokeBy nodeStanding`). -/
inductive Decision where
  | approve
  | deny
  | revoke
  deriving DecidableEq, Repr

/-- Denying withdraws only the denied request's grant: the member's current
membership, if any, stands. Revoking ends the membership and withdraws every
generation's grant (`revokeMember`). -/
def applyDecision (s : State) (request member : String) : Decision → State
  | .approve => grant s request member
  | .deny => revoke s request
  | .revoke => revokeMember s member

/-- An approval leaves its member holding every derived tuple. -/
theorem approve_derives_all (s : State) (request member : String)
    (fresh : s.receipts.any (·.request == request) = false) (t : Tuple) (d : t ∈ derived) :
    holds (applyDecision s request member .approve) member t = true :=
  grant_derives_all s request member fresh t d

/-- Denying a request no grant recorded changes nothing. -/
theorem deny_without_grant_changes_nothing (s : State) (request member : String)
    (h : liveReceipt s request = none) : applyDecision s request member .deny = s :=
  revoke_of_no_live s request h

/-! ### Executable fixtures -/

def ops : List Tuple := GrantSet.operatorUser.permissions.map Tuple.permission

def heldOf (s : State) (member : String) : List String :=
  (derived.filter (holds s member)).map Tuple.name

def fresh : State := { held := [⟨"owner", .nodeAdministrator⟩], receipts := [] }

/-- Granting and revoking one enrollment returns the member to what it held. -/
theorem grant_then_revoke_restores :
    let s := revoke (grant fresh "request" "member") "request"
    heldOf s "member" = [] ∧ heldOf s "owner" = ["node-administrator"] := by decide

/-- A permission the member held independently before the grant survives. -/
theorem prior_independent_permission_survives :
    let s0 := grantIndependent fresh ⟨"member", .permission "read-document"⟩
    let s := revoke (grant s0 "request" "member") "request"
    heldOf s "member" = ["read-document"] := by decide

/-- A tuple a second live enrollment also accounts for stays until both are
revoked. -/
theorem shared_tuple_waits_for_last_enrollment :
    let s1 := grant (grant fresh "a" "member") "b" "member"
    heldOf (revoke s1 "a") "member" = derived.map Tuple.name ∧
      heldOf (revoke (revoke s1 "a") "b") "member" = [] := by decide


/-- An approval grants exactly the enrolled-User authority: the Node
`administrator` relation and every `operatorUser` permission. -/
theorem approve_grants_node_administrator_and_operator_user :
    heldOf (applyDecision fresh "request" "member" .approve) "member" =
      "node-administrator" :: GrantSet.operatorUser.permissions := by decide

/-- Denying after a grant whose approval did not commit withdraws it. -/
theorem deny_withdraws_an_unapproved_grant :
    let s := applyDecision (applyDecision fresh "request" "member" .approve) "request" "member" .deny
    heldOf s "member" = [] ∧ heldOf s "owner" = ["node-administrator"] := by decide

/-- Denying a request nothing was granted for grants nothing. -/
theorem deny_grants_nothing :
    heldOf (applyDecision fresh "request" "member" .deny) "member" = [] := by decide

/-- Every point an approval's issuing can fail at. -/
def issuePoints : List Nat := List.range (derived.length + 1)

/-- An approval abandoned at any point leaves its member nothing, keeps a
permission the member held independently, and keeps what another approval
granted. -/
theorem abandoned_approval_leaves_no_authority :
    issuePoints.all (fun k =>
      heldOf (abandon (grantInterrupted fresh "request" "member" k) "request") "member" = [] &&
      heldOf (abandon (grantInterrupted
          (grantIndependent fresh ⟨"member", .permission "read-document"⟩) "request" "member" k)
          "request") "member" = ["read-document"] &&
      heldOf (abandon (grantInterrupted (grant fresh "other" "member") "request" "member" k)
          "request") "member" = derived.map Tuple.name) = true := by decide

/-- A retried approval after an abandoned one is exactly a clean approval,
and denying after an abandoned approval is exactly a denial of a clean one. -/
theorem retried_approval_after_abandon_is_a_clean_approval :
    issuePoints.all (fun k =>
      [fresh, grantIndependent fresh ⟨"member", .permission "read-document"⟩,
          grant fresh "other" "member"].all (fun s =>
        grant (abandon (grantInterrupted s "request" "member" k) "request") "request" "member" ==
            grant s "request" "member" &&
        revoke (abandon (grantInterrupted s "request" "member" k) "request") "request" ==
            revoke (grant s "request" "member") "request")) = true := by decide

/-- A renewed membership: revoking only the current request's receipt leaves
the first generation's receipt holding every derived tuple, which revoking
the membership withdraws. -/
theorem revoke_after_renewal_withdraws_every_generation :
    let s := grant (grant fresh "first" "member") "renewal" "member"
    heldOf (revoke s "renewal") "member" = derived.map Tuple.name ∧
      heldOf (revokeMember s "member") "member" = [] ∧
      heldOf (revokeMember s "member") "owner" = ["node-administrator"] := by decide

/-! ### Who may approve

The approver writes the member's Node `administrator` relation under its own
identity: a User through its signed client, an Agent through its managed
identity. Native document ACP admits that write only for the Node owner or a
holder of a relation that manages `administrator`, which the Household policy
gives `administrator` alone. So whether an approval proceeds is decided by
ACP and by nothing else: no surface, tool grant or operator signature widens
it. The Node key signs the decision only after the grant committed. Open
(#3681): whether an Agent that administers the Node, such as the Engineer, may
approve at all. -/

/-- The approver's standing on the Node an enrollment joins. -/
inductive Standing where
  | owner
  | administrator
  | runtimeReader
  | unrelated
  deriving DecidableEq, Repr

def Standing.relation : Standing → Option String
  | .administrator => some "administrator"
  | .runtimeReader => some "runtime_reader"
  | .owner | .unrelated => none

/-- A Node owned by `user`, with the approver holding its standing. -/
def standingState (standing : Standing) : Household.State :=
  { objects := [⟨"nodes", "node", if standing == .owner then "approver" else "user"⟩]
    relations := match standing.relation with
      | some relation => [⟨"nodes", "node", relation, .actor "approver"⟩]
      | none => []
    enrolled := [] }

/-- ACP admits the approver's write of the member's Node `administrator`
relation. External premise: the pinned DefraDB DAC relation management. -/
def grantAdmitted (standing : Standing) : Bool :=
  Household.mayManage (standingState standing) "approver"
    ⟨"nodes", "node", "administrator", .actor "member"⟩

/-- A named relation does not include the authority implied by ownership. -/
def holdsAdministratorRelation (state : Household.State) (member : String) : Bool :=
  Household.allowed 5 state member "nodes" "node" "administrator"

theorem administrator_observation_distinguishes_owner_from_direct_grant :
    holdsAdministratorRelation (standingState .owner) "approver" = false ∧
    holdsAdministratorRelation (standingState .administrator) "approver" = true ∧
    holdsAdministratorRelation (standingState .unrelated) "approver" = false ∧
    holdsAdministratorRelation (standingState .runtimeReader) "approver" = false := by
  native_decide

/-- A wildcard relation satisfies a named permission check without proving a
direct member tuple. A receipt must preserve the exact native tuple. -/
theorem wildcard_is_not_evidence_of_a_direct_administrator_tuple :
    let state : Household.State :=
      { objects := [⟨"nodes", "node", "owner"⟩]
        relations := [⟨"nodes", "node", "administrator", .wildcard⟩]
        enrolled := [] }
    (holdsAdministratorRelation state "member",
     state.relations.contains ⟨"nodes", "node", "administrator", .actor "member"⟩) =
      (true, false) := by native_decide

/-- Only an actor who can manage the administrator relation may observe a
member's exact administrator relationship. -/
def observeAdministrator (state : Household.State) (approver member : String) : Option Bool :=
  if Household.mayManage state approver
      ⟨"nodes", "node", "administrator", .actor member⟩ then
    some (state.relations.contains ⟨"nodes", "node", "administrator", .actor member⟩)
  else none

theorem administrator_observation_requires_native_management_authority :
    observeAdministrator (standingState .owner) "approver" "member" = some false ∧
    observeAdministrator (standingState .administrator) "approver" "approver" = some true ∧
    observeAdministrator (standingState .runtimeReader) "approver" "approver" = none ∧
    observeAdministrator (standingState .unrelated) "approver" "approver" = none := by
  native_decide

theorem exact_administrator_observation_excludes_wildcard :
    let state : Household.State :=
      { objects := [⟨"nodes", "node", "owner"⟩]
        relations := [⟨"nodes", "node", "administrator", .wildcard⟩]
        enrolled := [] }
    observeAdministrator state "owner" "member" = some false := by
  native_decide

/-- The approver's Node with the member's `administrator` relations: a direct
one naming the member, a wildcard one, both or neither. -/
def observationState (standing : Standing) (direct wildcard : Bool) : Household.State :=
  let base := standingState standing
  { base with relations := base.relations ++
      (if direct then [⟨"nodes", "node", "administrator", .actor "member"⟩] else []) ++
      (if wildcard then [⟨"nodes", "node", "administrator", .wildcard⟩] else []) }

/-- What the approver observes of the member's exact `administrator`
relationship (`observeAdministrator`): `none` when it may not manage the
relation, otherwise whether the direct tuple exists. A wildcard relation is
never a direct tuple. Rust owner: `ConfigAccess::observe_actor_relationship`. -/
def observeMemberAdministrator (standing : Standing) (direct wildcard : Bool) : Option Bool :=
  observeAdministrator (observationState standing direct wildcard) "approver" "member"

theorem a_wildcard_alone_is_not_observed_as_a_direct_administrator (standing : Standing) :
    observeMemberAdministrator standing false true ≠ some true := by
  cases standing <;> native_decide

/-- What an approval does once its grant has run as the approver. -/
inductive ApprovalStep where
  | approve
  | refuse
  deriving DecidableEq, Repr

/-- A committed grant is followed by the Node-signed approval; a refused or
failed grant approves nothing. -/
def afterGrant (grantCommitted : Bool) : ApprovalStep :=
  if grantCommitted then .approve else .refuse

def approval (standing : Standing) : ApprovalStep :=
  afterGrant (grantAdmitted standing)

/-- The Node owner and a Node administrator (the Engineer holds
`administrator`) approve; `runtime_reader` and an Agent with
no Node relation are refused by ACP, whatever its tool grant. -/
theorem approval_follows_node_administration :
    approval .owner = .approve ∧ approval .administrator = .approve ∧
    approval .runtimeReader = .refuse ∧
    approval .unrelated = .refuse := by native_decide

theorem refused_grant_approves_nothing : afterGrant false = .refuse := rfl

/-! ### Who may withdraw

Removing a member's Node `administrator` relation is admitted by native
document ACP on the rule that admits adding it (`Household.mayManage`): the
Node document's owner, or a holder of a relation that manages
`administrator`. The Node document is owned by the init User
(`NodeProvisioning.nacOwner`) and the Node holds `runtime_reader` on it and no
`bypass-dac` (`GrantSets.node_runtime_does_not_read_past_dac`), so the
runtime acting as itself can neither add nor remove the relation. Node
permissions are another matter: the runtime revokes them in process as the
NAC owner, as it issued them (`NodeProvisioning.nacOwner`).

So a withdrawal the runtime finds due on its own (the enrollment
reconciliation's denial or revocation) runs `oweRevoke` or
`oweRevokeMember`: it revokes the node permissions, retires the receipt and
marks it `owed`, in the revision's transaction; the member's `administrator`
relation stays until the owner's client issues it (`settle`), or a new
enrollment of the member takes it over (`grant`). An approver deciding as
itself (the client-access decision, `withdraw_as`) runs `revoke` directly.

What protects the home in between: the member holds no node permission the
receipt alone accounted for (`owing_withdraws_every_permission`), and native
node ACP, which the runtime requires enabled (`NodeProvisioning.runtimeMayStart`),
refuses every document read, write and relationship change of an actor
without those permissions, whatever its document relations; the revision that
retires the receipt also withdraws the member's routes and replication. What
remains exposed until the withdrawal is issued: the `administrator` relation
itself. The runtime grants the member no node permission meanwhile
(`permissionGrantAdmitted`), so it turns into access again only through a
node permission granted outside the runtime: natively over HTTP by the NAC
owner, or by a peer whose node ACP grants it against the same Vera
deployment. -/

/-- The Node's standing on its own Node document. -/
def nodeStanding : Standing := .runtimeReader

/-- Native admits the removal of a member's Node `administrator` relation on
the rule that admits adding it. External premise: the pinned DefraDB DAC
relation management decides deletes and adds by the same `manages` check. -/
def withdrawAdmitted (standing : Standing) : Bool := grantAdmitted standing

theorem withdrawal_follows_node_administration :
    withdrawAdmitted .owner = true ∧ withdrawAdmitted .administrator = true ∧
    withdrawAdmitted .runtimeReader = false ∧ withdrawAdmitted .unrelated = false := by
  native_decide

/-- The Node acting as itself may neither grant nor withdraw Node
administration. -/
theorem node_alone_neither_grants_nor_withdraws_administration :
    grantAdmitted nodeStanding = false ∧ withdrawAdmitted nodeStanding = false ∧
    approval nodeStanding = .refuse := by native_decide

/-- A due withdrawal of the member's Node `administrator` relation: revoking
would remove it if held. The runtime cannot observe the relation (only a
manager of it can, `observeAdministrator`), so it owes the withdrawal whether
or not the member still holds it; issuing the withdrawal of an absent relation
changes nothing. -/
def administrationDue (s : State) (r : Receipt) : Bool :=
  removedBy s r ⟨r.member, .nodeAdministrator⟩

/-- `revoke` run by an actor that may not remove the Node `administrator`
relation: every other tuple the receipt alone accounted for is removed, the
receipt is retired, and when the relation was due it is kept and the retired
receipt owes its withdrawal, in the same step. -/
def oweRevoke (s : State) (request : String) : State :=
  match liveReceipt s request with
  | none => s
  | some r =>
      { held := s.held.filter (fun a => !(removedBy s r a && a.tuple != .nodeAdministrator))
        receipts := (withdraw s.receipts request).map (fun x =>
          if x.request == request && administrationDue s r then { x with owed := true } else x) }

/-- The tuples revoking `member`'s membership removes (`revokeMember`). -/
def removedByMembership (s : State) (member : String) (a : Held) : Bool :=
  a.subject == member && s.receipts.any (accounts · member a.tuple)

/-- `revokeMember` run by an actor that may not remove the Node
`administrator` relation: the receipts of every generation are retired, and
those that accounted for the due relation owe its withdrawal. -/
def oweRevokeMember (s : State) (member : String) : State :=
  let due := removedByMembership s member ⟨member, .nodeAdministrator⟩
  { held := s.held.filter (fun a =>
      !(removedByMembership s member a && a.tuple != .nodeAdministrator))
    receipts := s.receipts.map (fun r =>
      if r.member == member then
        { r with withdrawn := true, owed := r.owed || (due && accounts r member .nodeAdministrator) }
      else r) }

/-- An owed withdrawal issued by `issuer`. Native refuses it from an actor that
may not manage the relation, which leaves everything as it was; otherwise
the relation is removed and the owed receipts are closed. -/
def settle (issuer : Standing) (s : State) (member : String) : State :=
  if withdrawAdmitted issuer && owedFor s member then
    { held := s.held.filter (· != ⟨member, .nodeAdministrator⟩)
      receipts := clearOwed s.receipts member }
  else s

/-- What an owed-settlement pass does about one member's owed withdrawal,
decided from the member's receipts. -/
inductive SettlementStep where
  /-- Remove the relation as the caller, under the credential its request
  carried, then decide again from the receipts read afresh (`afterRemoval`). -/
  | issue
  /-- A live receipt accounts for the relation: the member was approved again
  by an approval that did not take the withdrawal over. Close the owed
  receipts and keep the relation. -/
  | close
  | leave
  deriving DecidableEq, Repr

/-- Decision: the owner's client issues an owed withdrawal in the
owed-settlement pass it runs whenever it connects to its home
(`Identity.OwedRegistration`), on a local home as on a Vera home; the runtime
acts as the caller under the credential its request carried and signs
nothing. Native decides whether the caller may change the relation
(`withdrawAdmitted`); the pass asks it by observing the relation as the
caller first, which only a manager of the relation may do
(`observeAdministrator`), so a refused caller leaves the withdrawal owed and
is told nothing, since it is not the caller's to settle, and an absent
relation needs no removal. After removing the
relation the pass decides again from the receipts read afresh
(`afterRemoval`). So the relation ends held exactly when a live receipt
accounts for it; the interleaving with an approval is
`tla/SettlementApproval.tla`.
Rust owner: `ClientAccessOwner::settle_withdrawals`. -/
def settlementStep (owed takenOver : Bool) : SettlementStep :=
  if takenOver then (if owed then .close else .leave) else if owed then .issue else .leave

theorem settlement_removes_only_an_owed_unaccounted_relation (owed takenOver : Bool)
    (h : settlementStep owed takenOver = .issue) : owed = true ∧ takenOver = false := by
  cases owed <;> cases takenOver <;> simp_all [settlementStep]

theorem settlement_never_removes_a_relation_a_live_receipt_accounts_for (owed : Bool) :
    settlementStep owed true ≠ .issue := by
  cases owed <;> simp [settlementStep]

/-- What the pass does once it removed the relation, from the receipts read
afresh. -/
inductive AfterRemoval where
  /-- An approval recorded the relation while the pass ran: issue it again,
  as the caller. -/
  | reissue
  /-- Close the owed receipts. -/
  | close
  deriving DecidableEq, Repr

/-- An approval that took the relation over between the pass's first read and
its removal clears the owed receipts in its own receipt write, so only
whether a live receipt accounts for the relation decides. -/
def afterRemoval (takenOver : Bool) : AfterRemoval :=
  if takenOver then .reissue else .close

/-- Whatever an approval does while the pass runs, the relation ends held
exactly when a live receipt accounts for it. -/
theorem settlement_ends_held_exactly_when_accounted (takenOver : Bool) :
    (afterRemoval takenOver = .reissue) = (takenOver = true) := by
  cases takenOver <;> simp [afterRemoval]

/-- A withdrawal run by `issuer`: directly when it may remove the relation,
owed otherwise. -/
def revokeBy (issuer : Standing) (s : State) (request : String) : State :=
  if withdrawAdmitted issuer then revoke s request else oweRevoke s request

def revokeMemberBy (issuer : Standing) (s : State) (member : String) : State :=
  if withdrawAdmitted issuer then revokeMember s member else oweRevokeMember s member

/-- Decision (owner, gents#3979): the runtime grants no node permission to a
member whose administration removal is still owed, whatever grants it (a
grant set, a direct permission, a session share, an approval), so the
`administrator` relation left behind never turns into access again. An
enrollment grant of that member is admitted: its receipt write takes the
owed removal over before it issues any tuple
(`a_reapproval_closes_the_owed_removal_first`). Rust owner: the one funnel of
the runtime's node permission changes, `ConfigAccess::change_node_relation`,
which refuses with `ConfigClientError::AdministrationRemovalPending`. -/
def permissionGrantAdmitted (owed : Bool) : Bool := !owed

/-- A grant from outside any enrollment, checked by the runtime: a node
permission is refused to a member whose administration removal is owed
(`permissionGrantAdmitted`); the Node `administrator` relation, which only a
manager of it can add, is a deliberate grant that closes the owed removal.

Decision (owner, gents#3979): someone who may manage the relation and grants
it to a member whose removal is owed means it, so a later settlement must not
undo it. Rust owner: `ConfigAccess::add_actor_relationship` on the Node
`administrator` relation. -/
def grantOther (s : State) (a : Held) : State :=
  match a.tuple with
  | .permission _ =>
      if permissionGrantAdmitted (owedFor s a.subject) then grantIndependent s a else s
  | .nodeAdministrator => { grantIndependent s a with receipts := takeOver s.receipts a.subject }

/-! ### Expiry -/

/-- The clock an enrollment lease lapses on.

Decision (owner, gents#3979): a lapsed lease withdraws the grant exactly as a
revocation does. The provider's time is used where there is one, so the
runtime and the provider agree on when a lease ended; the node's wall clock
bounds it, so a provider that has not produced a block since the home came
back (its time stands where it stopped, `UserDelegation.idleTrailSeconds`)
cannot extend a lease. A home that was offline past a lease withdraws it on
its first enrollment sweep after it starts, not after the provider catches
up. External premise: the provider's time is in Unix seconds
(`node_provider_time`). -/
def lapseClock (provider : Option Nat) (wall : Nat) : Nat := max (provider.getD 0) wall

def lapsed (provider : Option Nat) (wall expiresAt : Nat) : Bool :=
  decide (expiresAt ≤ lapseClock provider wall)

/-- A membership lapses when every current authorization of the member has:
a renewal approved before expiry carries a later lease and keeps it. Rust
owner: `membership_lapsed`, run by the enrollment sweep under the decision
lock on a projection read under it, so a renewal and an expiry of the same
member are decided one after the other; an expiry withdrawn first leaves a
renewal approved after it a fresh enrollment, which takes the owed removal
over (`grant`). -/
def membershipLapsed (provider : Option Nat) (wall : Nat) (leases : List Nat) : Bool :=
  !leases.isEmpty && leases.all (lapsed provider wall)

theorem a_lease_never_outlives_either_clock (provider : Option Nat) (wall expiresAt : Nat)
    (live : lapsed provider wall expiresAt = false) :
    wall < expiresAt ∧ ∀ t, provider = some t → t < expiresAt := by
  simp only [lapsed, lapseClock, decide_eq_false_iff_not, Nat.not_le, Nat.max_lt] at live
  refine ⟨live.2, fun t h => ?_⟩
  simpa [h] using live.1

/-- A home back after its lease withdraws it whatever its provider's stale
time says. -/
theorem a_home_back_after_its_lease_withdraws (provider : Option Nat) (wall expiresAt : Nat)
    (past : expiresAt ≤ wall) : lapsed provider wall expiresAt = true := by
  simp only [lapsed, lapseClock, decide_eq_true_eq]
  exact Nat.le_trans past (Nat.le_max_right _ _)

theorem a_fresh_renewal_keeps_the_membership (provider : Option Nat) (wall renewal : Nat)
    (leases : List Nat) (renewed : renewal ∈ leases) (fresh : lapsed provider wall renewal = false) :
    membershipLapsed provider wall leases = false := by
  simp only [membershipLapsed, Bool.and_eq_false_iff, Bool.not_eq_false', List.all_eq_false]
  exact Or.inr ⟨renewal, renewed, by simp [fresh]⟩

/-! ### Restoration -/

/-- Some receipt owes the restoration of `member`'s Node `administrator`
relation (`takeOver`). -/
def restorationOwed (s : State) (member : String) : Bool :=
  s.receipts.any (fun r => r.restore && r.member == member)

def clearRestore (rs : List Receipt) (member : String) : List Receipt :=
  rs.map (fun r => if r.member == member then { r with restore := false } else r)

/-- What a settlement pass does about one member's owed restoration. -/
inductive RestorationStep where
  /-- Add the relation as the caller, then close the restoration. -/
  | restore
  /-- Close the restoration: the relation is held, or a later withdrawal owes
  its removal again, which supersedes the restoration. -/
  | close
  deriving DecidableEq, Repr

/-- Decision (owner, gents#3979 review): the pass observes the relation as
the caller, which only a manager of it may do, and restores it unless it is
held or its removal is owed again. Only a successful restore or such an
observation closes the restoration, so a failed restore leaves it owed for
the next pass. Rust owner: `settle_owed_withdrawals`. -/
def restorationStep (owed held : Bool) : RestorationStep :=
  if owed || held then .close else .restore

theorem restoration_closes_only_when_held_or_superseded (owed held : Bool)
    (h : restorationStep owed held = .close) : owed = true ∨ held = true := by
  cases owed <;> cases held <;> simp_all [restorationStep]

/-- A pass by `issuer` over `member`'s owed restoration. -/
def restoreBy (issuer : Standing) (s : State) (member : String) : State :=
  if withdrawAdmitted issuer && restorationOwed s member then
    { held := match restorationStep (owedFor s member) (holds s member .nodeAdministrator) with
        | .restore => s.held ++ [⟨member, .nodeAdministrator⟩]
        | .close => s.held
      receipts := clearRestore s.receipts member }
  else s

/-- The removal of a settlement pass that was already in flight when an
approval or a deliberate grant took the owed removal over: it lands after the
takeover and removes the relation the takeover wanted held. Only the
interleaving produces it (`tla/SettlementApproval.tla`); the traces replay it
to show the restoration repairs it. -/
def overtakingRemoval (s : State) (member : String) : State :=
  { s with held := s.held.filter (· != ⟨member, .nodeAdministrator⟩) }

/-- An approval or a deliberate grant that takes an owed removal over owes the
relation's restoration in the same step. -/
theorem a_takeover_owes_restoration (s : State) (request member : String)
    (fresh : s.receipts.any (·.request == request) = false) (owed : owedFor s member = true) :
    restorationOwed (grant s request member) member = true := by
  rw [grant, if_neg (by simp [fresh])]
  obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owed
  simp only [Bool.and_eq_true, beq_iff_eq] at hr
  simp only [restorationOwed, List.any_cons, takeOver, List.any_map]
  refine Bool.or_eq_true_iff.mpr (Or.inr (List.any_eq_true.mpr ⟨r, mem, ?_⟩))
  simp [hr.1, hr.2]

/-- A manager's pass leaves a member whose restoration is owed, and whose
removal is not owed again, holding the relation. -/
theorem a_managers_pass_restores (s : State) (issuer : Standing) (member : String)
    (admitted : withdrawAdmitted issuer = true) (owing : restorationOwed s member = true)
    (notOwed : owedFor s member = false) :
    holds (restoreBy issuer s member) member .nodeAdministrator = true := by
  unfold restoreBy
  rw [if_pos (by simp [admitted, owing])]
  simp only [holds, restorationStep, notOwed, Bool.false_or, List.contains_iff_mem]
  by_cases h : (⟨member, .nodeAdministrator⟩ : Held) ∈ s.held <;> simp [h]

theorem restore_of_map {rs : List Receipt} {f : Receipt → Receipt} {member : String}
    (keeps : ∀ r ∈ rs, (f r).member = r.member ∧ (r.restore = true → (f r).restore = true))
    (owing : rs.any (fun r => r.restore && r.member == member) = true) :
    (rs.map f).any (fun r => r.restore && r.member == member) = true := by
  obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owing
  simp only [Bool.and_eq_true, beq_iff_eq] at hr
  have ⟨m, o⟩ := keeps r mem
  refine List.any_eq_true.mpr ⟨f r, List.mem_map_of_mem _ mem, ?_⟩
  simp [o hr.1, m, hr.2]

/-- Every step that changes enrollment authority, with its actor where the
actor decides what it may change. An approval's grant and an abandoned
approval run as the approver, which ACP admits only when it manages the
relation (`approval`). An expiry runs as the Node, like the enrollment
reconciliation's revocation. -/
inductive Step where
  | grant (request member : String)
  | independent (a : Held)
  | revoke (issuer : Standing) (request : String)
  | revokeMember (issuer : Standing) (member : String)
  | expire (member : String)
  | abandon (request : String)
  | settle (issuer : Standing) (member : String)
  | restore (issuer : Standing) (member : String)
  deriving DecidableEq, Repr

def Step.apply (s : State) : Step → State
  | .grant request member => Identity.EnrollmentGrant.grant s request member
  | .independent a => grantOther s a
  | .revoke issuer request => revokeBy issuer s request
  | .revokeMember issuer member => revokeMemberBy issuer s member
  | .expire member => revokeMemberBy nodeStanding s member
  | .abandon request => Identity.EnrollmentGrant.abandon s request
  | .settle issuer member => Identity.EnrollmentGrant.settle issuer s member
  | .restore issuer member => restoreBy issuer s member

theorem owed_of_map {rs : List Receipt} {f : Receipt → Receipt} {member : String}
    (keeps : ∀ r ∈ rs, (f r).member = r.member ∧ (r.owed = true → (f r).owed = true))
    (owed : rs.any (fun r => r.owed && r.member == member) = true) :
    (rs.map f).any (fun r => r.owed && r.member == member) = true := by
  obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owed
  simp only [Bool.and_eq_true, beq_iff_eq] at hr
  have ⟨m, o⟩ := keeps r mem
  refine List.any_eq_true.mpr ⟨f r, List.mem_map_of_mem _ mem, ?_⟩
  simp [o hr.1, m, hr.2]

theorem owed_after_withdraw {rs : List Receipt} {request member : String}
    (owed : rs.any (fun r => r.owed && r.member == member) = true) :
    (withdraw rs request).any (fun r => r.owed && r.member == member) = true :=
  owed_of_map (fun r _ => by by_cases h : r.request == request <;> simp [h]) owed

theorem owed_after_clear_of_other {rs : List Receipt} {other member : String}
    (differs : other ≠ member) (owed : rs.any (fun r => r.owed && r.member == member) = true) :
    (clearOwed rs other).any (fun r => r.owed && r.member == member) = true := by
  obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owed
  simp only [Bool.and_eq_true, beq_iff_eq] at hr
  refine List.any_eq_true.mpr ⟨if r.member == other then { r with owed := false } else r,
    List.mem_map_of_mem _ mem, ?_⟩
  have ne : ¬ member = other := fun h => differs h.symm
  simp [hr.1, hr.2, ne]

theorem owed_after_takeOver_of_other {rs : List Receipt} {other member : String}
    (differs : other ≠ member) (owed : rs.any (fun r => r.owed && r.member == member) = true) :
    (takeOver rs other).any (fun r => r.owed && r.member == member) = true := by
  obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owed
  simp only [Bool.and_eq_true, beq_iff_eq] at hr
  refine List.any_eq_true.mpr ⟨if r.member == other && r.owed then
      { r with owed := false, restore := true } else r, List.mem_map_of_mem _ mem, ?_⟩
  have ne : ¬ member = other := fun h => differs h.symm
  simp [hr.1, hr.2, ne]

theorem owed_after_grant_of_other (s : State) (request other member : String)
    (differs : other ≠ member) (owed : owedFor s member = true) :
    owedFor (grant s request other) member = true := by
  unfold grant
  split
  · split
    · exact owed
    · exact owed
  · simp only [owedFor, List.any_cons]
    rw [owed_after_takeOver_of_other differs owed, Bool.or_true]

theorem owed_after_independent (s : State) (a : Held) (member : String)
    (owed : owedFor s member = true) : owedFor (grantIndependent s a) member = true := by
  unfold grantIndependent
  split
  · exact owed
  · exact owed

theorem owed_after_other_grant (s : State) (a : Held) (member : String)
    (owed : owedFor s member = true)
    (notAdministration : a.subject ≠ member ∨ a.tuple ≠ .nodeAdministrator) :
    owedFor (grantOther s a) member = true := by
  unfold grantOther
  split
  · split
    · exact owed_after_independent s a member owed
    · exact owed
  · rename_i h
    rcases notAdministration with other | notAdmin
    · exact owed_after_takeOver_of_other other owed
    · exact absurd h notAdmin

theorem owed_after_revoke (s : State) (request member : String)
    (owed : owedFor s member = true) : owedFor (revoke s request) member = true := by
  unfold revoke
  split
  · exact owed
  · exact owed_after_withdraw owed

theorem owed_after_oweRevoke (s : State) (request member : String)
    (owed : owedFor s member = true) : owedFor (oweRevoke s request) member = true := by
  unfold oweRevoke
  split
  · exact owed
  · rename_i r _
    exact owed_of_map (fun x _ => by
      by_cases h : x.request == request && administrationDue s r <;> simp [h])
      (owed_after_withdraw owed)

theorem owed_after_revokeMember (s : State) (issuer : Standing) (m member : String)
    (owed : owedFor s member = true) : owedFor (revokeMemberBy issuer s m) member = true := by
  unfold revokeMemberBy
  split
  · exact owed_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owed
  · exact owed_of_map (fun r _ => by
      by_cases h : r.member == m
      · simp_all
      · simp [h]) owed

/-- An owed withdrawal is never dropped: it stays owed until a manager of the
relation issues it or a new enrollment of the member takes it over. -/
theorem owed_withdrawal_is_never_dropped (s : State) (member : String) (step : Step)
    (owed : owedFor s member = true) (gone : owedFor (step.apply s) member = false) :
    (∃ issuer, step = .settle issuer member ∧ withdrawAdmitted issuer = true) ∨
      (∃ request, step = .grant request member) ∨
      step = .independent ⟨member, .nodeAdministrator⟩ := by
  cases step with
  | grant request m =>
      by_cases hm : m = member
      · exact Or.inr (Or.inl ⟨request, by rw [hm]⟩)
      · rw [Step.apply, owed_after_grant_of_other s request m member hm owed] at gone
        exact absurd gone (by decide)
  | independent a =>
      by_cases deliberate : a.subject = member ∧ a.tuple = .nodeAdministrator
      · obtain ⟨subject, tuple⟩ := deliberate
        right; right
        cases a; simp_all
      · have : a.subject ≠ member ∨ a.tuple ≠ .nodeAdministrator := by
          by_cases h : a.subject = member
          · exact Or.inr fun t => deliberate ⟨h, t⟩
          · exact Or.inl h
        rw [Step.apply, owed_after_other_grant s a member owed this] at gone
        exact absurd gone (by decide)
  | expire m =>
      rw [Step.apply, owed_after_revokeMember s nodeStanding m member owed] at gone
      exact absurd gone (by decide)
  | revoke issuer request =>
      have : owedFor (Step.apply s (.revoke issuer request)) member = true := by
        simp only [Step.apply, revokeBy]
        split
        · exact owed_after_revoke s request member owed
        · exact owed_after_oweRevoke s request member owed
      rw [this] at gone
      exact absurd gone (by decide)
  | revokeMember issuer m =>
      rw [Step.apply, owed_after_revokeMember s issuer m member owed] at gone
      exact absurd gone (by decide)
  | abandon request =>
      have : owedFor (abandon s request) member = true := by
        simp only [owedFor, abandon_keeps_receipts]; exact owed
      rw [Step.apply, this] at gone
      exact absurd gone (by decide)
  | restore issuer m =>
      have : owedFor (restoreBy issuer s m) member = true := by
        unfold restoreBy
        split
        · exact owed_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owed
        · exact owed
      rw [Step.apply, this] at gone
      exact absurd gone (by decide)
  | settle issuer m =>
      by_cases admitted : withdrawAdmitted issuer = true
      · by_cases hm : m = member
        · exact Or.inl ⟨issuer, by rw [hm], admitted⟩
        · have : owedFor (settle issuer s m) member = true := by
            unfold settle
            split
            · exact owed_after_clear_of_other hm owed
            · exact owed
          rw [Step.apply, this] at gone
          exact absurd gone (by decide)
      · have : settle issuer s m = s := by
          simp [settle, Bool.eq_false_iff.mpr admitted]
        rw [Step.apply, this, owed] at gone
        exact absurd gone (by decide)

/-- No step the Node takes as itself removes a member's Node `administrator`
relation: its denials and revocations owe the withdrawal, and its settlement
is refused. With `node_alone_neither_grants_nor_withdraws_administration`, no
path lets the Node grant or remove Node administration by itself. -/
theorem node_withdrawal_keeps_administration (s : State) (member : String) (step : Step)
    (byNode : (∃ request, step = .revoke nodeStanding request) ∨
      (∃ m, step = .revokeMember nodeStanding m) ∨ (∃ m, step = .expire m) ∨
      (∃ m, step = .settle nodeStanding m))
    (holding : holds s member .nodeAdministrator = true) :
    holds (step.apply s) member .nodeAdministrator = true := by
  have refused : withdrawAdmitted nodeStanding = false := by native_decide
  simp only [holds, List.contains_iff_mem] at holding ⊢
  rcases byNode with ⟨request, rfl⟩ | ⟨m, rfl⟩ | ⟨m, rfl⟩ | ⟨m, rfl⟩
  · simp only [Step.apply, revokeBy, refused, Bool.false_eq_true, if_false]
    unfold oweRevoke
    split
    · exact holding
    · exact List.mem_filter.mpr ⟨holding, by simp⟩
  · simp only [Step.apply, revokeMemberBy, refused, Bool.false_eq_true, if_false]
    exact List.mem_filter.mpr ⟨holding, by simp [oweRevokeMember]⟩
  · simp only [Step.apply, revokeMemberBy, refused, Bool.false_eq_true, if_false]
    exact List.mem_filter.mpr ⟨holding, by simp [oweRevokeMember]⟩
  · simp only [Step.apply, settle, refused, Bool.false_and, Bool.false_eq_true, if_false]
    exact holding

/-- When the Node finds a member's administration withdrawal due, the same
step retires the receipt and owes the withdrawal: no state has one without
the other. -/
theorem owing_retires_and_owes_together (s : State) (request : String) (r : Receipt)
    (live : liveReceipt s request = some r) (due : administrationDue s r = true)
    (holding : holds s r.member .nodeAdministrator = true) :
    liveReceipt (oweRevoke s request) request = none ∧ owedFor (oweRevoke s request) r.member = true ∧
      holds (oweRevoke s request) r.member .nodeAdministrator = true := by
  have ⟨mem, req, _⟩ := liveReceipt_spec live
  have unfolded : oweRevoke s request =
      { held := s.held.filter (fun a => !(removedBy s r a && a.tuple != .nodeAdministrator))
        receipts := (withdraw s.receipts request).map (fun x =>
          if x.request == request && administrationDue s r then { x with owed := true } else x) } := by
    unfold oweRevoke; rw [live]
  rw [unfolded]
  refine ⟨?_, ?_, ?_⟩
  · simp only [liveReceipt]
    rw [List.find?_eq_none]
    intro x hx
    simp only [List.mem_map] at hx
    obtain ⟨y, hy, rfl⟩ := hx
    simp only [withdraw, List.mem_map] at hy
    obtain ⟨z, _, rfl⟩ := hy
    by_cases hz : z.request == request
    · by_cases hd : administrationDue s r <;> simp [hz, hd]
    · simp [hz]
  · simp only [owedFor]
    refine List.any_eq_true.mpr ⟨{ r with withdrawn := true, owed := true }, ?_, by simp⟩
    simp only [List.mem_map, withdraw]
    refine ⟨{ r with withdrawn := true }, ⟨r, mem, by simp [req]⟩, by simp [req, due]⟩
  · simp only [holds, List.contains_iff_mem] at holding ⊢
    exact List.mem_filter.mpr ⟨holding, by simp⟩

/-- Owing the withdrawal still withdraws, at once, every node permission the
receipt alone accounted for: what native node ACP gates the member on. -/
theorem owing_withdraws_every_permission (s : State) (request : String) (r : Receipt)
    (live : liveReceipt s request = some r) (p : String)
    (recorded : r.tuples.contains (.permission p) = true)
    (alone : (withdraw s.receipts r.request).any (accounts · r.member (.permission p)) = false) :
    holds (oweRevoke s request) r.member (.permission p) = false := by
  simp only [oweRevoke, live, holds, removedBy]
  rw [Bool.eq_false_iff]
  intro hc
  simp only [List.contains_iff_mem, List.mem_filter] at hc recorded
  have hr := hc.2
  simp [recorded, alone] at hr

/-- A membership revocation the Node finds due owes the withdrawal of the
member's administration on its retired receipts and withdraws every node
permission at once. -/
theorem owing_a_membership_keeps_only_administration :
    let s := oweRevokeMember (grant (grant fresh "first" "member") "renewal" "member") "member"
    heldOf s "member" = ["node-administrator"] ∧ owedFor s "member" = true ∧
      heldOf (settle .owner s "member") "member" = [] ∧
      heldOf (settle .runtimeReader s "member") "member" = ["node-administrator"] := by
  native_decide

/-- A member re-approved while its withdrawal is owed is administrator by the
new enrollment, and revoking that one withdraws the relation. -/
theorem reapproval_takes_the_owed_withdrawal_over :
    let s := grant (oweRevoke (grant fresh "a" "member") "a") "b" "member"
    owedFor s "member" = false ∧ accounted s "member" .nodeAdministrator = true ∧
      heldOf (revoke s "b") "member" = [] := by
  native_decide


/-- A member whose administration removal is owed gains no node permission
from any grant outside an enrollment. -/
theorem an_owed_member_gains_no_node_permission (s : State) (member p : String)
    (owed : owedFor s member = true) :
    grantOther s ⟨member, .permission p⟩ = s := by
  simp [grantOther, permissionGrantAdmitted, owed]

/-- A fresh enrollment grant of the member writes its receipt and closes the
owed removal in one step, before it issues any tuple, so the permission rule
admits its tuples. -/
theorem a_reapproval_closes_the_owed_removal_first (s : State) (request member : String)
    (fresh : s.receipts.any (·.request == request) = false) :
    owedFor (grant s request member) member = false := by
  rw [grant, if_neg (by simp [fresh])]
  simp only [owedFor, List.any_cons, takeOver, List.any_map, Bool.false_and, Bool.false_or]
  rw [List.any_eq_false]
  intro r _
  by_cases h : r.member == member <;> by_cases o : r.owed <;> simp_all

/-- A deliberate grant of the relation by a manager closes the owed removal,
so a later settlement keeps the relation. -/
theorem a_deliberate_grant_closes_the_owed_removal (s : State) (member : String) :
    owedFor (grantOther s ⟨member, .nodeAdministrator⟩) member = false ∧
      settle .owner (grantOther s ⟨member, .nodeAdministrator⟩) member =
        grantOther s ⟨member, .nodeAdministrator⟩ := by
  have closed : owedFor (grantOther s ⟨member, .nodeAdministrator⟩) member = false := by
    simp only [grantOther, owedFor, takeOver, List.any_map]
    rw [List.any_eq_false]
    intro r _
    by_cases h : r.member == member <;> by_cases o : r.owed <;> simp_all
  exact ⟨closed, by simp [settle, closed]⟩

/-- An expired member holds no node permission its grants alone accounted
for, from the moment of the expiry. -/
theorem expiry_withdraws_every_permission (s : State) (member p : String)
    (accountedFor : accounted s member (.permission p) = true) :
    holds (Step.apply s (.expire member)) member (.permission p) = false := by
  have refused : withdrawAdmitted nodeStanding = false := by native_decide
  simp only [Step.apply, revokeMemberBy, refused, Bool.false_eq_true, if_false, holds,
    oweRevokeMember, removedByMembership]
  rw [Bool.eq_false_iff]
  intro h
  simp only [accounted] at accountedFor
  simp [List.contains_iff_mem, List.mem_filter, accountedFor] at h

/-- An expiry owes the administration removal like a revocation; a renewal
approved after it is a fresh enrollment and takes the removal over. -/
theorem expiry_owes_administration_and_a_later_renewal_takes_it_over :
    let s := Step.apply (grant fresh "first" "member") (.expire "member")
    heldOf s "member" = ["node-administrator"] ∧ owedFor s "member" = true ∧
      owedFor (grant s "renewal" "member") "member" = false ∧
      heldOf (grant s "renewal" "member") "member" = derived.map Tuple.name := by
  native_decide


theorem restore_after_takeOver {rs : List Receipt} {m member : String}
    (owing : rs.any (fun r => r.restore && r.member == member) = true) :
    (takeOver rs m).any (fun r => r.restore && r.member == member) = true := by
  unfold takeOver
  exact restore_of_map (fun r _ => by by_cases h : (r.member == m && r.owed) = true <;> simp [h])
    owing

theorem restoration_owed_of_map (s : State) (rs : List Receipt) (f : Receipt → Receipt)
    (member : String) (heldList : List Held) (same : rs = s.receipts.map f)
    (keeps : ∀ r ∈ s.receipts, (f r).member = r.member ∧ (r.restore = true → (f r).restore = true))
    (owing : restorationOwed s member = true) :
    restorationOwed { held := heldList, receipts := rs } member = true := by
  subst same; exact restore_of_map keeps owing

/-- An owed restoration is never dropped: it stays owed until a pass of a
manager of the relation closes it (`restorationStep`), whatever else happens
meanwhile, approvals, revocations, expiries and settlements included. So an
approved member whose relation a settlement removed is restored or still owed
its restoration, never silently left without it. -/
theorem restoration_is_never_dropped (s : State) (member : String) (step : Step)
    (owing : restorationOwed s member = true)
    (gone : restorationOwed (step.apply s) member = false) :
    ∃ issuer, step = .restore issuer member ∧ withdrawAdmitted issuer = true := by
  by_cases exception : ∃ issuer, step = .restore issuer member ∧ withdrawAdmitted issuer = true
  · exact exception
  have kept : restorationOwed (step.apply s) member = true := by
    cases step with
    | grant request m =>
        simp only [Step.apply, grant]
        split
        · split
          · exact owing
          · exact owing
        · simp only [restorationOwed, List.any_cons]
          rw [restore_after_takeOver owing, Bool.or_true]
    | independent a =>
        simp only [Step.apply, grantOther]
        split
        · split
          · simp only [grantIndependent]; split <;> exact owing
          · exact owing
        · exact restore_after_takeOver owing
    | revoke issuer request =>
        simp only [Step.apply, revokeBy]
        split
        · unfold revoke; split
          · exact owing
          · exact restore_of_map (fun r _ => by by_cases h : r.request == request <;> simp [h]) owing
        · unfold oweRevoke; split
          · exact owing
          · rename_i r _
            exact restore_of_map (fun x _ => by
              by_cases h : x.request == request && administrationDue s r <;> simp [h])
              (restore_of_map (fun r _ => by by_cases h : r.request == request <;> simp [h]) owing)
    | revokeMember issuer m =>
        simp only [Step.apply, revokeMemberBy]
        split
        · exact restore_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owing
        · exact restore_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owing
    | expire m =>
        simp only [Step.apply, revokeMemberBy]
        split
        · exact restore_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owing
        · exact restore_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owing
    | abandon request =>
        simp only [Step.apply, restorationOwed, abandon_keeps_receipts]; exact owing
    | settle issuer m =>
        simp only [Step.apply, settle]
        split
        · exact restore_of_map (fun r _ => by by_cases h : r.member == m <;> simp [h]) owing
        · exact owing
    | restore issuer m =>
        have closing : ¬ (m = member ∧ withdrawAdmitted issuer = true) :=
          fun ⟨h, admitted⟩ => exception ⟨issuer, by rw [h], admitted⟩
        · simp only [Step.apply, restoreBy]
          split
          · rename_i hif
            have admitted : withdrawAdmitted issuer = true := by
              simp only [Bool.and_eq_true] at hif; exact hif.1
            have other : m ≠ member := fun h => closing ⟨h, admitted⟩
            obtain ⟨r, mem, hr⟩ := List.any_eq_true.mp owing
            simp only [Bool.and_eq_true, beq_iff_eq] at hr
            refine List.any_eq_true.mpr ⟨if r.member == m then { r with restore := false } else r,
              List.mem_map_of_mem _ mem, ?_⟩
            have ne : ¬ member = m := fun h => other h.symm
            simp [hr.1, hr.2, ne]
          · exact owing
  rw [kept] at gone
  exact absurd gone (by decide)

end Identity.EnrollmentGrant

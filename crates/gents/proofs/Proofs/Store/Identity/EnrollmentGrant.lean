import Proofs.Store.Identity.GrantSets
import Proofs.Store.Identity.Household

/-!
The authority an explicit enrollment grant derives for one verified client
enrollment, and what revoking that enrollment withdraws.

Rust owner: `crates/gents-store/src/identity/enrollment_grant.rs`
(`EnrollmentGrantTuple`, `plan_grant`, `plan_withdrawal`, `approve_enrollment`),
written by every enrollment approval and by `gents p2p enrollment grant`, and
withdrawn by
`GraphqlEnrollmentStore::revoke_request` in the transaction that publishes the
revocation revision.
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
and revocation withdraws it in the revision's transaction. -/
structure Receipt where
  request : String
  member : String
  tuples : List Tuple
  withdrawn : Bool := false
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

/-- Held by another, independent grant: present natively and accounted for by
no live enrollment receipt. The owner's own authority (Node ownership and the
native `admin` relation) is never a derived tuple and is independent. -/
def independent (s : State) (member : String) (t : Tuple) : Bool :=
  holds s member t && !accounted s member t

/-- A grant records every derived tuple except those held independently. A
tuple another live enrollment already accounts for is recorded again, so it
stays held until the last enrollment accounting for it is revoked. -/
def record (s : State) (member : String) : List Tuple :=
  derived.filter (fun t => !independent s member t)

/-- One receipt per request: a repeated grant of the same request is
idempotent. -/
def grant (s : State) (request member : String) : State :=
  if s.receipts.any (·.request == request) then s
  else
    { held := s.held ++ ((record s member).filter (fun t => !holds s member t)).map (⟨member, ·⟩)
      receipts := ⟨request, member, record s member, false⟩ :: s.receipts }

/-- A grant from any other source. Native tuples are a set: granting a held
tuple changes nothing and leaves no provenance. External premise: the pinned
DefraDB DAC relationship store and NAC actor relationships. -/
def grantIndependent (s : State) (a : Held) : State :=
  if s.held.contains a then s else { s with held := a :: s.held }

def withdraw (rs : List Receipt) (request : String) : List Receipt :=
  rs.map (fun r => if r.request == request then { r with withdrawn := true } else r)

def liveReceipt (s : State) (request : String) : Option Receipt :=
  s.receipts.find? (fun r => r.request == request && !r.withdrawn)

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
    exact indep.1
  have notAcc : accounted s subject t = false := by
    simp only [independent, Bool.and_eq_true, Bool.not_eq_true'] at indep
    exact indep.2
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
  refine ⟨indep.1, ?_⟩
  by_cases hs : subject = member
  · subst hs; simp [indep.2]
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
finds the receipt and reissues it (`grant` is idempotent per request). Denying
runs `revoke`, so a grant whose approval never committed is withdrawn and no
derived authority outlives a denial. Rust owners: `approve_enrollment` (grant,
then approve) and the runtime enrollment store's denial, which withdraws the
request's live receipt. -/
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

end Identity.EnrollmentGrant

import Proofs.Store.AssignmentReceiver
import Proofs.Store.DocumentAcpChoice

/-! Which Agents a runtime's desired state holds (gents#4245). An Agent's
owner places it on a node with the Agent document's `host` relationship
(`Identity.Household.policy`). The assignment receiver is the only reader of
certified placements: per issuer scope it records the view it accepted (the
floor) before stopping what that view withdraws, and the view whose stops have
finished (the applied view). It starts and stops nothing itself; the control
watcher reads both rows and the agent supervisor runs what this decision
admits, so the supervisor stays the only owner of what runs. The interleaving
is `tla/AssignmentSupervisor.tla`. -/

namespace RuntimeReconcile.Placement

open DocumentAcpChoice (Provider)

inductive Decision where
  /-- The Agent is in the desired generation. -/
  | run
  /-- The Agent is placed here but its configuration is not verified yet; it
  is listed unavailable, waiting for its configuration. -/
  | waiting
  /-- The Agent is not in the desired generation. -/
  | absent
  deriving DecidableEq, Repr

/-- Decision: on a Vera home an Agent runs only when the floor and the applied
view of its issuer's scope both hold it, that view is fresh, and the Agent's
body (its document and the configuration closure it selects) is verified.
Requiring the floor keeps a withdrawal in force across a restart before the
receiver finishes it; requiring the applied view starts a replacement only
after the withdrawals the floor owes have stopped. Ownership alone starts
nothing: creation places every Agent on its creating node. A home on the
node's own store has no certified reads and decides as before placement:
an Agent runs when its row names this node. -/
def desired (home : Provider) (ownedHere floor applied bodyVerified fresh : Bool) : Decision :=
  match home with
  | .nodeStore => if ownedHere then .run else .absent
  | .embeddedVera =>
      if !(floor && applied && fresh) then .absent
      else if AssignmentReceiver.permits true bodyVerified then .run
      else .waiting

theorem a_desired_agent_is_placed_and_verified (ownedHere floor applied bodyVerified fresh : Bool)
    (h : desired .embeddedVera ownedHere floor applied bodyVerified fresh = .run) :
    floor = true ∧ applied = true ∧ bodyVerified = true ∧ fresh = true := by
  cases floor <;> cases applied <;> cases bodyVerified <;> cases fresh <;>
    simp_all [desired, AssignmentReceiver.permits]

theorem absent_from_the_floor_is_not_desired (ownedHere applied bodyVerified fresh : Bool) :
    desired .embeddedVera ownedHere false applied bodyVerified fresh = .absent := by
  simp [desired]

theorem ownership_alone_starts_nothing_on_a_vera_home (bodyVerified fresh : Bool) :
    desired .embeddedVera true false false bodyVerified fresh = .absent := by
  simp [desired]

theorem a_placed_agent_without_a_verified_body_waits (ownedHere : Bool) :
    desired .embeddedVera ownedHere true true false true = .waiting := by
  simp [desired, AssignmentReceiver.permits]

theorem a_local_store_home_decides_as_today (ownedHere floor applied bodyVerified fresh : Bool) :
    desired .nodeStore ownedHere floor applied bodyVerified fresh =
      if ownedHere then .run else .absent := rfl

/-- How a node's document access control treats a replicated update or
delete before merging it. External premise (DefraDB): a node whose document
access control is a Vera home holds the verified signer of every replicated
update and delete to that home's `update` or `delete` permission when it
merges the change, and neither merges a refused change nor one to a document
that is not registered yet; a first commit is authorized by the document's
registration. A node deciding access in its own store merges an unregistered
replica unchecked. -/
inductive MergeCheck where
  | unchecked
  /-- Every replicated change is held to the policy of `home`. -/
  | strict (home : String)
  deriving DecidableEq, Repr

/-- One document of an Agent's body, the Agent document or one its
configuration selects (`references_of`, transitively), as the transaction
that read the placement rows saw it. `head` is the document's head. -/
structure ClosureDocument where
  present : Bool
  registered : Bool
  head : String
  deriving DecidableEq, Repr

inductive Body where
  /-- This node does not hold replicated changes to the issuer's home, so
  nothing it reads is known to be authorized. -/
  | notStrict
  /-- A document of the closure has not arrived or is not registered yet. -/
  | waiting
  /-- Every document of the closure is present and registered; a slot started
  from this body binds `heads`. -/
  | verified (heads : List String)
  deriving DecidableEq, Repr

/-- Decision: an Agent's body is verified when this node holds every
replicated change to the issuer's home, and the Agent document and every
document its configuration selects are present and registered. Authorization
happens when a change is written: the node merged each commit of those
documents only after the home allowed its signer, so the host does not check
signatures again against the current state, and a commit by an editor allowed
at merge stays valid after that editor is revoked. A refused change is not
merged, so the heads read are the last authorized version. A missing or
unregistered document is waited for, as a reference that has not arrived is,
never refused. The placement pins no version: an allowed edit moves a head,
the slot bound to the old heads no longer matches its configuration
(`SurfacePublication.recreateSlot`) and the edit applies at once. Within an
account the issuer's home is the node's own Vera home; a placement issued
from another account's home names that home as `issuerHome`. -/
def body (check : MergeCheck) (issuerHome : String) (closure : List ClosureDocument) : Body :=
  match check with
  | .unchecked => .notStrict
  | .strict home =>
      if home != issuerHome then .notStrict
      else if closure.all fun document => document.present && document.registered then
        .verified (closure.map (·.head))
      else .waiting

def Body.isVerified : Body → Bool
  | .verified _ => true
  | _ => false

theorem a_desired_agent_runs_an_authorized_body (ownedHere floor applied fresh : Bool)
    (check : MergeCheck) (issuerHome : String) (closure : List ClosureDocument)
    (h : desired .embeddedVera ownedHere floor applied
      (body check issuerHome closure).isVerified fresh = .run) :
    check = .strict issuerHome ∧
      (∀ document ∈ closure, document.present = true ∧ document.registered = true) ∧
      body check issuerHome closure = .verified (closure.map (·.head)) := by
  have verified := (a_desired_agent_is_placed_and_verified _ _ _ _ _ h).2.2.1
  cases check with
  | unchecked => simp [body, Body.isVerified] at verified
  | strict home =>
      by_cases same : home = issuerHome
      · by_cases complete : closure.all fun document => document.present && document.registered
        · refine ⟨by rw [same], ?_, by simp [body, same, complete]⟩
          intro document member
          have := List.all_eq_true.mp complete document member
          simpa using this
        · simp [body, same, complete, Body.isVerified] at verified
      · simp [body, same, Body.isVerified] at verified

theorem an_unchecked_node_never_verifies_a_body (issuerHome : String)
    (closure : List ClosureDocument) : body .unchecked issuerHome closure = .notStrict := rfl

theorem a_node_strict_at_another_home_never_verifies_a_body (home issuerHome : String)
    (closure : List ClosureDocument) (h : home ≠ issuerHome) :
    body (.strict home) issuerHome closure = .notStrict := by
  simp [body, h]

theorem a_missing_document_leaves_the_body_waiting (home : String) (registered : Bool)
    (head : String) (before after : List ClosureDocument) :
    body (.strict home) home (before ++ ⟨false, registered, head⟩ :: after) = .waiting := by
  simp [body]

theorem an_unregistered_document_leaves_the_body_waiting (home : String) (present : Bool)
    (head : String) (before after : List ClosureDocument) :
    body (.strict home) home (before ++ ⟨present, false, head⟩ :: after) = .waiting := by
  simp [body]

theorem a_changed_head_changes_a_verified_body (home : String)
    (before after : List ClosureDocument) (head head' : String) (h : head ≠ head')
    (verified : (body (.strict home) home (before ++ ⟨true, true, head⟩ :: after)).isVerified) :
    body (.strict home) home (before ++ ⟨true, true, head⟩ :: after) ≠
      body (.strict home) home (before ++ ⟨true, true, head'⟩ :: after) := by
  by_cases complete : (before ++ after).all fun document => document.present && document.registered
  · have all : ∀ head, ((before ++ ⟨true, true, head⟩ :: after).all fun document =>
        document.present && document.registered) = true := by
      intro head
      simp only [List.all_append, List.all_cons, Bool.and_self, Bool.true_and] at complete ⊢
      exact complete
    simp [body, all, h]
  · have incomplete : ((before ++ ⟨true, true, head⟩ :: after).all fun document =>
        document.present && document.registered) = false := by
      simp only [List.all_append, List.all_cons, Bool.and_self, Bool.true_and] at complete ⊢
      simpa using complete
    simp [body, incomplete, Body.isVerified] at verified

/-- Whether the floor a view read has reached `floor`. Certified revisions are
ordered by term, then height; the receiver refuses two views at one height
that differ (`AssignmentReceiver.accepts`), so the hash does not order them. -/
def reached (basis floor : AssignmentReceiver.Position) : Bool :=
  floor.term < basis.term || (floor.term == basis.term && floor.height ≤ basis.height)

/-- The floor a view read for `scope`, per issuer scope, in the transaction
that read the Agents. -/
abbrev Basis := List (String × AssignmentReceiver.Position)

def readFloor (basis : Basis) (scope : String) (floor : AssignmentReceiver.Position) : Bool :=
  (List.lookup scope basis).any (reached · floor)

/-- Decision: the supervisor admits a proposal only when, for every scope its
placement fence holds, the floor the proposal's view read has reached the
fence. The receiver raises the fence right after it commits a floor, so a
proposal built from a view older than that floor, still queued or still being
built, is dropped instead of starting the agents that floor withdraws; the
watcher resolves again. A scope the view did not read counts as below the
fence, since its floor row was committed after the view's transaction. The
fence lives in memory: after a restart nothing is queued and the supervisor
bootstraps from the durable rows (`tla/AssignmentSupervisor.tla`, `Crash`). -/
def admitsProposal (basis fence : Basis) : Bool :=
  fence.all fun (scope, floor) => readFloor basis scope floor

inductive Outcome where
  /-- Below the fence: nothing changes and the watcher resolves again. -/
  | drop
  /-- The configuration is unchanged: the generation stays and only the
  published basis moves to the proposal's. -/
  | advanceBasis
  /-- A new generation is built from the proposal. -/
  | apply
  deriving DecidableEq, Repr

/-- Decision: what the supervisor does with a proposal. An admitted proposal
whose configuration matches the running generation still advances the basis
it publishes, or the stop barrier would wait for ever on a withdrawal that
changed no configuration (`tla/AssignmentSupervisor.tla`, `NoopKeepsBasis`).
A parked slot rebuilds the generation as before. -/
def proposalOutcome (admitted fingerprintChanged parked : Bool) : Outcome :=
  if !admitted then .drop
  else if fingerprintChanged || parked then .apply
  else .advanceBasis

/-- Decision: the stop barrier passes once the supervisor has published a
basis that reached the floor for the scope, and no withdrawn Agent is in the
published generation or in a retired slot that has not joined. Proposals are
taken in order and the floor only rises, so no proposal the supervisor takes
afterwards holds a withdrawn Agent. -/
def stopped (basis : Basis) (scope : String) (floor : AssignmentReceiver.Position)
    (ids generation retiring : List String) : Bool :=
  readFloor basis scope floor && ids.all fun id => !generation.contains id && !retiring.contains id

theorem an_empty_fence_admits_every_proposal (basis : Basis) :
    admitsProposal basis [] = true := rfl

theorem a_proposal_that_did_not_read_a_fenced_scope_is_dropped (scope : String)
    (floor : AssignmentReceiver.Position) (fence : Basis) :
    admitsProposal [] ((scope, floor) :: fence) = false := by
  simp [admitsProposal, readFloor]

theorem a_proposal_below_the_fence_is_dropped (scope : String) (term height : Nat)
    (hash hash' : String) :
    admitsProposal [(scope, ⟨term, height, hash⟩)] [(scope, ⟨term, height + 1, hash'⟩)] = false := by
  simp [admitsProposal, readFloor, reached]

theorem a_dropped_proposal_is_not_applied (fingerprintChanged parked : Bool) :
    proposalOutcome false fingerprintChanged parked = .drop := rfl

theorem an_unchanged_admitted_proposal_advances_the_basis :
    proposalOutcome true false false = .advanceBasis := rfl

theorem the_barrier_waits_for_the_basis (scope : String) (floor : AssignmentReceiver.Position)
    (ids generation retiring : List String) :
    stopped [] scope floor ids generation retiring = false := by
  simp [stopped, readFloor]

theorem the_barrier_waits_for_a_retiring_slot (basis : Basis) (scope id : String)
    (floor : AssignmentReceiver.Position) (generation : List String) :
    stopped basis scope floor [id] generation [id] = false := by
  simp [stopped]

end RuntimeReconcile.Placement

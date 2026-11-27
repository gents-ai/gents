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

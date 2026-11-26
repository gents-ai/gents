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

end RuntimeReconcile.Placement

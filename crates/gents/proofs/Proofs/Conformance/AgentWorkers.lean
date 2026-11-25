import Proofs.Runtime.CanonicalOutput.Execution.WorkerLevels
import Proofs.Conformance.Family

namespace Conformance.AgentWorkers

open Lean CanonicalOutput.Execution

structure HeldTurnsInput where
  backendLimit : Nat
  children : Nat
  deriving ToJson

structure HeldTurnsObservation where
  workers : Nat
  running : Nat
  atServer : Nat
  queued : Nat
  peak : Nat
  sessionTitleStarted : Bool
  deriving ToJson

/-- How many requests hold a worker once a turn at hop zero and the
`children` delegates it started at hop one are dispatched to one Agent, in
that order. -/
def admitted (children : Nat) : Nat :=
  let requests := (0, 0) :: (List.range children).map fun index => (1, index + 1)
  (requests.foldl
    (fun (state : WorkerLevels.Pools × Nat) (request : Nat × Nat) =>
      match WorkerLevels.admit state.1 request.1 (request.2, 1) with
      | some pools => (pools, state.2 + 1)
      | none => state)
    (WorkerLevels.initial, 0)).2

/-- A turn starts `children` delegates of its own Agent and makes its next
model call; every turn's call is held at the server. Each request holds a
worker of its level, the backend admits `backendLimit` of the calls and queues
the rest, and the server never holds more than it admits. The title audit of
the turn's own session, the second request on that session, starts while the
turn is open. Every case holds at least as many calls as the backend admits,
so a session-title call cannot raise the peak above the held calls. -/
@[conformance_family]
def heldTurns : Family HeldTurnsInput HeldTurnsObservation where
  inputs := [⟨1, 1⟩, ⟨2, 2⟩]
  model := fun i =>
    let running := admitted i.children
    let atServer := min running i.backendLimit
    { workers := WorkerLevels.workers
      running
      atServer
      queued := running - atServer
      peak := atServer
      sessionTitleStarted := WorkerLevels.lane true 0 == .auxiliary }

theorem every_case_fills_the_backend :
    heldTurns.inputs.all (fun i => i.backendLimit ≤ admitted i.children) = true := by
  native_decide

theorem every_delegate_starts_beside_its_turn :
    heldTurns.inputs.all (fun i => admitted i.children = i.children + 1) = true := by
  native_decide

end Conformance.AgentWorkers

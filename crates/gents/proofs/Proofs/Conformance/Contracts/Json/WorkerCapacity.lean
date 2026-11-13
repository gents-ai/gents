import Proofs.Runtime.CanonicalOutput.Execution.WorkerCapacity
import Proofs.Conformance.Family

namespace Conformance.WorkerCapacityContracts

open CanonicalOutput.Execution.WorkerCapacity
open Lean

inductive Operation where
  | acquire (ticket : Ticket)
  | release (ticket : Ticket)

structure Case where
  name : String
  pre : State
  operation : Operation

def Case.evaluate (c : Case) : Option State :=
  match c.operation with
  | .acquire ticket => CanonicalOutput.Execution.WorkerCapacity.acquire c.pre ticket
  | .release ticket => some (CanonicalOutput.Execution.WorkerCapacity.release c.pre ticket)

def casesOption : Option (List Case) := do
  let empty := initial 1
  let held ← acquire empty (10, 7)
  pure
    [ ⟨"acquire_free_worker", empty, .acquire (10, 7)⟩
    , ⟨"full_capacity_refuses_second_request", held, .acquire (11, 8)⟩
    , ⟨"duplicate_ticket_refused", held, .acquire (10, 7)⟩
    , ⟨"release_frees_worker", held, .release (10, 7)⟩
    , ⟨"zero_capacity_refuses", initial 0, .acquire (10, 7)⟩ ]

private def fixtureTickets : List Ticket := [(10, 7), (11, 8)]

inductive OperationKind where
  | acquire
  | release
  deriving ToJson

structure TicketWire where
  doc : Nat
  generation : Nat
  deriving ToJson

structure StateWire where
  activeLimit : Nat
  active : List TicketWire
  deriving ToJson

structure OperationWire where
  kind : OperationKind
  ticket : TicketWire
  deriving ToJson

structure TransitionInput where
  name : String
  pre : StateWire
  operation : OperationWire
  deriving ToJson

def TicketWire.ofModel (ticket : Ticket) : TicketWire := ⟨ticket.1, ticket.2⟩

def TicketWire.toModel (ticket : TicketWire) : Ticket := (ticket.doc, ticket.generation)

def StateWire.ofModel (state : State) : StateWire :=
  ⟨state.activeLimit, (fixtureTickets.filter (· ∈ state.active)).map TicketWire.ofModel⟩

def StateWire.toModel (state : StateWire) : State :=
  ⟨state.activeLimit, (state.active.map TicketWire.toModel).toFinset⟩

def OperationWire.ofModel : Operation → OperationWire
  | .acquire ticket => ⟨.acquire, TicketWire.ofModel ticket⟩
  | .release ticket => ⟨.release, TicketWire.ofModel ticket⟩

def OperationWire.toModel (operation : OperationWire) : Operation :=
  match operation.kind with
  | .acquire => .acquire operation.ticket.toModel
  | .release => .release operation.ticket.toModel

@[conformance_family]
def transition : Family TransitionInput (Option StateWire) where
  inputs := (casesOption.getD []).map fun c =>
    { name := c.name, pre := StateWire.ofModel c.pre, operation := OperationWire.ofModel c.operation }
  model := fun i =>
    (Case.evaluate ⟨i.name, i.pre.toModel, i.operation.toModel⟩).map StateWire.ofModel

theorem cases_reachable : casesOption.isSome = true := by native_decide

private def represented (state : State) : Bool :=
  decide (state.active ⊆ fixtureTickets.toFinset)

theorem serialized_fixture_tickets_cover_all_states :
    (casesOption.getD []).all (fun c =>
      represented c.pre && c.evaluate.all represented) = true := by native_decide

theorem cases_have_expected_success_and_refusal :
    ((casesOption.getD []).filter (fun c => c.evaluate.isSome)).length = 2 ∧
      ((casesOption.getD []).filter (fun c => c.evaluate.isNone)).length = 3 := by
  native_decide

end Conformance.WorkerCapacityContracts

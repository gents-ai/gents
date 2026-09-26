import Proofs.CanonicalOutput.Execution.WorkerCapacity
import Proofs.Conformance.Contracts.Json.Helpers

/-! Executable request-worker operations. Expectations are evaluations of the
same owner functions used in the scheduler proof, not copied fixture results. -/
namespace Conformance.WorkerCapacityContracts

open CanonicalOutput.Execution.WorkerCapacity
open Conformance.Contracts

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

private def ticketJson (ticket : Ticket) : String :=
  "[" ++ toString ticket.1 ++ "," ++ toString ticket.2 ++ "]"

private def fixtureTickets : List Ticket := [(10, 7), (11, 8)]

private def stateJson (state : State) : String :=
  "{" ++ "\"active_limit\":" ++ toString state.activeLimit ++ ","
    ++ "\"active\":" ++ jsonArray ((fixtureTickets.filter (· ∈ state.active)).map ticketJson) ++ "}"

private def optionalStateJson : Option State → String
  | none => "null"
  | some state => stateJson state

private def operationJson : Operation → String
  | .acquire ticket => "{\"kind\":\"acquire\",\"ticket\":" ++ ticketJson ticket ++ "}"
  | .release ticket => "{\"kind\":\"release\",\"ticket\":" ++ ticketJson ticket ++ "}"

def caseJson (c : Case) : String :=
  "{" ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"pre\":" ++ stateJson c.pre ++ ","
    ++ "\"operation\":" ++ operationJson c.operation ++ ","
    ++ "\"expected\":" ++ optionalStateJson c.evaluate ++ "}"

def casesJson : String := jsonArray ((casesOption.getD []).map caseJson)

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

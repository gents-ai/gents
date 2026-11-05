import Proofs.Client.Types
import Proofs.Conformance.ContractTypes

namespace Conformance.ClientSessionDelivery

open Conformance.Contracts

def requestStates : List RequestState :=
  [ .workspaceBindingPending, .pending, .claimed, .processing, .completed
  , .failed, .superseded, .dead, .interrupted ]

theorem requestStates_complete (state : RequestState) : state ∈ requestStates := by
  cases state <;> simp [requestStates]

def submittedAttempts : List (Option SubmittedAttempt) :=
  none :: requestStates.flatMap fun lifecycleState =>
    [false, true].flatMap fun isSuperseded =>
      [false, true].map fun claimed =>
        some { request := { lifecycleState, isSuperseded }, claimed }

def pairings : List PairingSignal := [.connected, .notConnected]

def sessionRows : List SessionRowObservation :=
  [.absent, .canonical, .duplicate, .outOfScope]

/-- Every combination of the four signals, over every lifecycle state. -/
def views : List SessionDeliveryView :=
  sessionRows.flatMap fun sessionRow =>
    pairings.flatMap fun pairing =>
      submittedAttempts.map fun submitted => { submitted, pairing, sessionRow }

def pairingJson : PairingSignal → String
  | .connected => "connected"
  | .notConnected => "not_connected"

def sessionRowJson : SessionRowObservation → String
  | .absent => "absent"
  | .canonical => "canonical"
  | .duplicate => "duplicate"
  | .outOfScope => "out_of_scope"

def deliveryJson : SessionDelivery → String
  | .visible => "visible"
  | .accepted => "accepted"
  | .delivering => "delivering"
  | .notConnected => "not_connected"
  | .noRequest => "no_request"
  | .integrityFault => "integrity_fault"

def submittedJson : Option SubmittedAttempt → String
  | none => "null"
  | some attempt =>
    "{\"lifecycle_state\":" ++ jsonString attempt.request.lifecycleState.toDefraDB ++
    ",\"is_superseded\":" ++ toString attempt.request.isSuperseded ++
    ",\"claimed\":" ++ toString attempt.claimed ++ "}"

def caseJson (view : SessionDeliveryView) : String :=
  "{\"submitted\":" ++ submittedJson view.submitted ++
  ",\"pairing\":" ++ jsonString (pairingJson view.pairing) ++
  ",\"session_row\":" ++ jsonString (sessionRowJson view.sessionRow) ++
  ",\"expected\":" ++ jsonString (deliveryJson (deriveSessionDelivery view)) ++ "}"

def casesJson : String := jsonArray (views.map caseJson)

def directReadJson : DirectSessionRead → String
  | .empty => "empty"
  | .visible => "visible"
  | .integrityFault => "integrity_fault"

def directCaseJson (row : SessionRowObservation) : String :=
  "{\"session_row\":" ++ jsonString (sessionRowJson row) ++
  ",\"expected\":" ++ jsonString (directReadJson (directSessionRead row)) ++ "}"

def directCasesJson : String := jsonArray (sessionRows.map directCaseJson)

end Conformance.ClientSessionDelivery

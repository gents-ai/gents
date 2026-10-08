import Proofs.Session.FoldCases
import Proofs.CanonicalOutput.Execution.HandoverCases
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.FoldQueueContracts

open SessionQueue SessionQueue.FoldCases Conformance.Contracts

def entryJson (entry : QueueEntry) : String :=
  "{\"request_id\":" ++ toString entry.requestId ++
  ",\"execution_origin\":" ++ jsonString entry.origin.toDefraDB ++
  ",\"source\":" ++ jsonString entry.source.toDefraDB ++
  ",\"policy\":" ++ jsonString entry.policy.toDefraDB ++
  ",\"queued_after\":" ++ jsonOptionalNat entry.queuedAfter ++
  ",\"requester_id\":" ++ jsonOptionalNat entry.requester ++
  ",\"turn_context\":" ++ toString entry.turnContext ++ "}"

def eventJson : Event → String
  | .enqueue entry => "{\"kind\":\"enqueue\",\"entry\":" ++ entryJson entry ++ "}"
  | .claim admitted =>
      "{\"kind\":\"claim\",\"admitted\":" ++ jsonArray (admitted.map toString) ++ "}"
  | .consume => "{\"kind\":\"consume\"}"
  | .finish => "{\"kind\":\"finish\"}"

def caseJson (entry : String × List Event) : String :=
  "{\"name\":" ++ jsonString entry.1 ++
  ",\"agent_id\":" ++ toString queue.scope.agent ++
  ",\"session_id\":" ++ toString queue.scope.session ++
  ",\"inputs\":" ++ jsonArray (entry.2.map eventJson) ++
  ",\"expected\":" ++ (match observation entry.2 with
    | none => "null"
    | some observed =>
        "{\"active\":" ++ jsonOptionalNat observed.active ++
        ",\"pending\":" ++ jsonArray (observed.pending.map toString) ++
        ",\"folding\":" ++ jsonArray (observed.folding.map toString) ++
        ",\"terminal\":" ++ jsonArray (observed.terminal.map toString) ++
        ",\"claims\":" ++ jsonArray (observed.claims.map fun (head, folded) =>
          "{\"head\":" ++ toString head ++
          ",\"folded\":" ++ jsonArray (folded.map toString) ++ "}") ++ "}") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

def authoredKeyJson : AuthoredKey → String
  | .context => "{\"kind\":\"context\"}"
  | .prompt => "{\"kind\":\"prompt\"}"
  | .folded id => "{\"kind\":\"folded\",\"request_id\":" ++ toString id ++ "}"

def turnInputCaseJson (entry : String × TurnInput String) : String :=
  let input := entry.2
  "{\"name\":" ++ jsonString entry.1 ++
  ",\"context\":" ++ (match input.context with | none => "null" | some c => jsonString c) ++
  ",\"head\":" ++ jsonString input.head ++
  ",\"folded\":" ++ jsonArray (input.folded.map fun (id, content) =>
    "{\"request_id\":" ++ toString id ++ ",\"content\":" ++ jsonString content ++ "}") ++
  ",\"authored\":" ++ jsonArray (input.authored.map fun (key, content) =>
    "{\"key\":" ++ authoredKeyJson key ++ ",\"content\":" ++ jsonString content ++ "}") ++
  ",\"provider_input\":" ++ jsonArray (input.providerInput.map jsonString) ++ "}"

def turnInputCasesJson : String := jsonArray (turnInputCases.map turnInputCaseJson)

open CanonicalOutput.Execution.Handover.Cases in
def handoverCaseJson (value : FoldClaimCase) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"pending\":" ++ jsonArray (value.pending.map entryJson) ++
  ",\"admitted\":" ++ jsonArray (value.admitted.map toString) ++
  ",\"expected\":" ++ (match foldClaim value with
    | none => "null"
    | some queue =>
        "{\"active\":" ++ jsonOptionalNat queue.active ++
        ",\"folding\":" ++ jsonArray (queue.folding.map (toString ·.requestId)) ++
        ",\"pending\":" ++ jsonArray (queue.pending.map (toString ·.requestId)) ++ "}") ++ "}"

def handoverCasesJson : String :=
  jsonArray (CanonicalOutput.Execution.Handover.Cases.foldClaimCases.map handoverCaseJson)

end Conformance.FoldQueueContracts

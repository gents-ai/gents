import Proofs.Session.FoldCases
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
        ",\"claims\":" ++ jsonArray (observed.claims.map fun (head, folded) =>
          "{\"head\":" ++ toString head ++
          ",\"folded\":" ++ jsonArray (folded.map toString) ++ "}") ++ "}") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.FoldQueueContracts

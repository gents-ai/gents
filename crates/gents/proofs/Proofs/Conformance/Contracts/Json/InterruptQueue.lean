import Proofs.Session.InterruptCases
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.InterruptQueueContracts

open SessionQueue SessionQueue.InterruptCases Conformance.Contracts

def entryJson (entry : QueueEntry) : String :=
  "{\"request_id\":" ++ toString entry.requestId ++
  ",\"created_at\":" ++ toString entry.createdAt ++
  ",\"execution_origin\":" ++ jsonString entry.origin.toDefraDB ++
  ",\"source\":" ++ jsonString entry.source.toDefraDB ++
  ",\"policy\":" ++ jsonString entry.policy.toDefraDB ++
  ",\"queue_key\":" ++ jsonOptionalNat entry.queueKey ++
  ",\"queued_after\":" ++ jsonOptionalNat entry.queuedAfter ++ "}"

def eventJson : Event → String
  | .interrupt => "{\"kind\":\"interrupt\"}"
  | .captureInterrupt => "{\"kind\":\"capture_interrupt\"}"
  | .commitInterrupt => "{\"kind\":\"commit_interrupt\"}"
  | .enqueue entry => "{\"kind\":\"enqueue\",\"entry\":" ++ entryJson entry ++ "}"

def caseJson (entry : String × List Event) : String :=
  "{\"name\":" ++ jsonString entry.1 ++
  ",\"agent_id\":" ++ toString queue.scope.agent ++
  ",\"requester_id\":" ++ jsonOptionalNat queue.scope.requester ++
  ",\"session_id\":" ++ toString queue.scope.session ++
  ",\"active_request_id\":" ++ jsonOptionalNat queue.active ++
  ",\"inputs\":" ++ jsonArray (entry.2.map eventJson) ++
  ",\"expected\":" ++ (match observation entry.2 with
    | none => "null"
    | some (pending, terminal, latched) =>
        "{\"pending\":" ++ jsonArray (pending.map toString) ++
        ",\"terminal\":" ++ jsonArray (terminal.map toString) ++
        ",\"latched\":" ++ jsonOptionalBool (some latched) ++ "}") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.InterruptQueueContracts

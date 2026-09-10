import Proofs.Conformance.ContractCases.R6Background
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Lean

namespace Conformance.Contracts
open Lean Conformance.ContractCases BackgroundCompletion

private def wakeContextJson (c : WakeRequest) : Json := Json.mkObj
  [("state", toJson c.state.toDefraDB), ("origin", toJson c.origin.toDefraDB),
   ("admission", toJson (match c.admission with
     | .released => "released" | .waiting => "waiting"
     | .acquired => "acquired" | .executing => "executing")),
   ("retry_count", toJson c.retryCount), ("max_retries", toJson c.maxRetries),
   ("depth", toJson c.depth), ("parent_request_id", toJson c.parentRequestId),
   ("deadline", toJson c.deadline)]

private def failedWakeJson (wake : FailedWake) : Json := Json.mkObj
  [("request_id", toJson wake.requestId), ("context", wakeContextJson wake.ctx),
   ("source", toJson wake.source.toDefraDB), ("policy", toJson wake.policy.toDefraDB),
   ("queue_key", toJson wake.queueKey)]

private def wakeRowsCaseJson (c : WakeRowsCase) : Json := Json.mkObj
  [("name", toJson c.name), ("goal", toJson (c.goal.map Goals.Status.toDefraDB)),
   ("session", documentJson c.session), ("rows", toJson (c.rows.map requestJson)),
   ("parent_doc_id", toJson c.parentDoc), ("wake", failedWakeJson c.wake),
   ("successor", requestJson c.successor), ("normalized_preview", toJson "wake"),
   ("now", toJson (3 : Nat)),
   ("publication", c.result.map (fun post => Json.mkObj
     [("successor_context", wakeContextJson post.1), ("session", documentJson post.2.1),
      ("rows", toJson (post.2.2.map requestJson))]) |>.getD Json.null)]

def backgroundWakeRowsCasesJson : String :=
  (toJson (backgroundWakeRowsCases.map wakeRowsCaseJson)).compress

end Conformance.Contracts

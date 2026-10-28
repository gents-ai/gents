import Proofs.DurableLineage
import Lean

open Lean DurableLineage

private def kindName : TriggerOrigin → String
  | .event => "event"
  | .schedule => "schedule"
  | .manual => "manual"
  | .goal => "goal"
  | .unknown => "unknown"

def main : IO Unit := do
  let decisions := ["caller", "other"].flatMap fun target =>
    [none, some "caller", some "other"].flatMap fun origin =>
      [false, true].flatMap fun sameNode =>
        [false, true].map fun taskTriggeredAtCreation =>
          Json.mkObj [
            ("caller", toJson ("caller" : String)),
            ("target", toJson target),
            ("origin", toJson origin),
            ("sameNode", toJson sameNode),
            ("taskTriggeredAtCreation", toJson taskTriggeredAtCreation),
            ("allowed", toJson (interruptAllowed
              "caller" target origin sameNode taskTriggeredAtCreation))]
  let evidence := [false, true].flatMap fun exactScope =>
    [false, true].flatMap fun isCreationRequest =>
      [false, true].flatMap fun hasTriggerId =>
        [false, true].flatMap fun hasTriggerDoc =>
          [TriggerOrigin.event, .schedule, .manual, .goal, .unknown].map fun kind =>
            let e : TaskTriggerEvidence :=
              ⟨exactScope, isCreationRequest, hasTriggerId, hasTriggerDoc, kind⟩
            Json.mkObj [
              ("exactScope", toJson exactScope),
              ("isCreationRequest", toJson isCreationRequest),
              ("hasTriggerId", toJson hasTriggerId),
              ("hasTriggerDoc", toJson hasTriggerDoc),
              ("kind", toJson (kindName kind)),
              ("verified", toJson (verifiedTaskTrigger e)),
              ("allowed", toJson (interruptAllowed "caller" "other" none true
                (verifiedTaskTrigger e)))]
  IO.println (Json.mkObj [("decisions", toJson decisions), ("evidence", toJson evidence)]).compress

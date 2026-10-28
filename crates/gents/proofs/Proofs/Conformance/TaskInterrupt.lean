import Proofs.DurableLineage
import Proofs.AgentSession
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
        [false, true].flatMap fun taskTriggeredAtCreation =>
          [false, true].map fun hasParentDoc =>
          Json.mkObj [
            ("caller", toJson ("caller" : String)),
            ("target", toJson target),
            ("origin", toJson origin),
            ("sameNode", toJson sameNode),
            ("taskTriggeredAtCreation", toJson taskTriggeredAtCreation),
            ("hasParentDoc", toJson hasParentDoc),
            ("allowed", toJson (interruptAllowed
              "caller" target origin sameNode taskTriggeredAtCreation hasParentDoc))]
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
  let creation := [none, some (12 : Nat)].flatMap fun parent =>
    [false, true].flatMap fun hasCallingTool =>
      [false, true].flatMap fun admitted =>
       [false, true].map fun automatedTrigger =>
        let provenance := AgentSession.requestCreationProvenance 25 parent hasCallingTool admitted automatedTrigger
        Json.mkObj [
          ("requestDoc", toJson (25 : Nat)), ("parentDoc", toJson parent),
          ("hasCallingTool", toJson hasCallingTool), ("admitted", toJson admitted),
          ("automatedTrigger", toJson automatedTrigger),
          ("hasProvenance", toJson provenance.isSome),
          ("openingRequestDoc", toJson (provenance.bind fun p => p.openingRequestDoc)),
          ("parentRequestDoc", toJson (provenance.bind fun p => p.parentRequestDoc))]
  IO.println (Json.mkObj [("decisions", toJson decisions), ("evidence", toJson evidence),
    ("creation", toJson creation)]).compress

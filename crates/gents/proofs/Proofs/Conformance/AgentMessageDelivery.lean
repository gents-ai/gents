import Lean
import Proofs.Request.AgentMessageDelivery

open Lean AgentMessageDelivery

private def turnName : TurnOutcome → String
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"

private def senderName : SenderOutcome → String
  | .pending => "pending"
  | .completed => "completed"
  | .failed => "failed"
  | .interrupted => "interrupted"

private def modeName : Mode → String
  | .queue => "queue"
  | .steer => "steer"
  | .interrupt => "interrupt"

private def targetName : Target → String
  | .idle => "idle"
  | .active => "active"
  | .unknown => "unknown"

private def decisionName : Decision → String
  | .start => "start"
  | .queue => "queue"
  | .steer => "steer"
  | .interrupt => "interrupt"
  | .unavailable => "unavailable"
  | .denied => "denied"

def main : IO Unit := do
  let decisions := [Mode.queue, .steer, .interrupt].flatMap fun mode =>
    [Target.idle, .active, .unknown].flatMap fun target =>
      [false, true].flatMap fun intake =>
        [false, true].flatMap fun linked =>
          [false, true].map fun allowed =>
            Json.mkObj [("mode", toJson (modeName mode)), ("target", toJson (targetName target)),
              ("intake", toJson intake), ("completion_linked", toJson linked),
              ("interrupt_allowed", toJson allowed),
              ("expected", toJson (decisionName (decide mode target intake linked allowed)))]
  let cases := [false, true].flatMap fun edge =>
    [false, true].flatMap fun head =>
      [TurnOutcome.running, .completed, .failed, .interrupted].map fun outcome =>
        Json.mkObj [("caller_edge_matches", toJson edge), ("consuming_request_matches", toJson head),
          ("turn_outcome", toJson (turnName outcome)),
          ("expected", toJson ((foldedSenderOutcome edge head outcome).map senderName))]
  let requesters := [false, true].flatMap fun agent =>
    [false, true].flatMap fun same =>
      [false, true].map fun edge =>
        Json.mkObj [("agent", toJson agent), ("same_requester", toJson same),
          ("complete_parent_edge", toJson edge), ("expected", toJson (requesterAllowed agent same edge))]
  let contexts := [false, true].flatMap fun agent =>
    [false, true].flatMap fun exact =>
      [false, true].map fun empty =>
        Json.mkObj [("agent", toJson agent), ("exact_context", toJson exact),
          ("empty_agent_context", toJson empty), ("expected", toJson (contextAllowed agent exact empty))]
  let claimStates := [false, true].flatMap fun selfTarget =>
    [TargetClaimState.unknown, .pending, .active, .terminal].map fun state =>
      let stateName := match state with
        | .unknown => "unknown" | .pending => "pending" | .active => "active" | .terminal => "terminal"
      let decisionName := match targetedClaimFor selfTarget state with
        | .wait => "wait" | .reject => "reject"
      Json.mkObj [("self_target", toJson selfTarget), ("state", toJson stateName), ("expected", toJson decisionName)]
  let targets := ["first", "second"].flatMap fun requested =>
    ["first", "second"].map fun head =>
      Json.mkObj [("requested", toJson requested), ("head", toJson head),
        ("expected", toJson (targetsHead requested head))]
  IO.println ((Json.mkObj [("decisions", toJson decisions), ("folded_outcomes", toJson cases),
    ("requesters", toJson requesters), ("contexts", toJson contexts), ("targets", toJson targets),
    ("targeted_claims", toJson claimStates)]).compress)

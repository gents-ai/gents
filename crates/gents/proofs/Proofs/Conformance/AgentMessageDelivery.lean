import Proofs.Session.Management
import Lean

open Lean SessionQueue

private def modeName : AgentMessageMode → String
  | .queue => "queue"
  | .steer => "steer"
  | .interrupt => "interrupt"

private def deliveryName : QueueDelivery → String
  | .queue => "queue"
  | .steer => "steer"

def main : IO Unit := do
  let cases := [AgentMessageMode.queue, .steer, .interrupt].flatMap fun mode =>
    [false, true].map fun allowed =>
      let result := agentMessageDelivery? mode allowed
      Json.mkObj [
        ("mode", toJson (modeName mode)),
        ("mayInterrupt", toJson allowed),
        ("delivery", toJson (result.map fun pair => deliveryName pair.1)),
        ("interrupt", toJson (result.map Prod.snd))]
  IO.println (toJson cases).compress

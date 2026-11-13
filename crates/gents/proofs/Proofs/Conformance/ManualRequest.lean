import Proofs.Session.AgentSession
import Proofs.Conformance.Family

namespace Conformance.ManualRequest

open Lean AgentSession

structure EnqueueInput where
  conversationTitle : Option String
  deriving ToJson

structure EnqueueOutput where
  initialTitle : Option String
  requestBindsSession : Bool
  sessionMaterialized : Bool
  sessionTitle : Option String
  deriving ToJson

@[conformance_family]
def enqueue : Family EnqueueInput EnqueueOutput where
  inputs := [
    { conversationTitle := some "mini-host-health-20260430t180405z" },
    { conversationTitle := some "   " },
    { conversationTitle := none }]
  model := fun i =>
    let title := (requestTitle i.conversationTitle).map (·.text)
    { initialTitle := title
      requestBindsSession := true
      sessionMaterialized := enqueueMaterializesSession
      sessionTitle := if enqueueMaterializesSession then title else none }

end Conformance.ManualRequest

import Proofs.Session.Compaction.DurableReduction
import Proofs.Conformance.Family

namespace Conformance.SourceBoundary

open Lean Compaction.DurableReduction

structure MessageWire where
  node : Nat
  session : Nat
  requester : Option Nat
  sequence : Int
  deriving ToJson

structure HighWaterInput where
  node : Nat
  session : Nat
  requester : Option Nat
  messages : List MessageWire
  deriving ToJson

def MessageWire.model (m : MessageWire) : TranscriptMessage :=
  ⟨m.node, m.session, m.requester, m.sequence⟩

def message (node : Nat) (sequence : Int) (session : Nat := 1) (requester : Option Nat := none) :
    MessageWire :=
  { node, session, requester, sequence }

@[conformance_family]
def highWater : Family HighWaterInput (Option Int) where
  inputs :=
    [ { node := 1, session := 1, requester := none,
        messages := [message 1 2, message 1 1, message 2 3] }
    , { node := 1, session := 1, requester := none, messages := [] }
    , { node := 1, session := 1, requester := none, messages := [message 2 7] }
    , { node := 1, session := 1, requester := none,
        messages := [message 1 (-5), message 1 (-1), message 1 (-3)] }
    , { node := 1, session := 1, requester := some 4,
        messages := [message 1 9, message 1 2 (requester := some 4), message 1 6 (requester := some 5)] }
    , { node := 1, session := 1, requester := none,
        messages := [message 1 4 (session := 2), message 1 0] } ]
  model := fun i => canonicalHighWater i.node i.session i.requester (i.messages.map (·.model))

end Conformance.SourceBoundary

import Proofs.Tools.MCPHealth.Shutdown
import Proofs.Conformance.Family

namespace Conformance.MCPHealthShutdown

open Lean Proofs.MCPHealth Proofs.MCPHealth.Shutdown

deriving instance ToJson for Shutdown

structure Input where
  known : List String
  registry : List String
  failing : Nat
  threshold : Nat
  shutdown : Shutdown
  deriving ToJson

structure Entry where
  service : String
  state : String
  failureCount : Nat
  deriving ToJson

structure Output where
  probesStarted : Nat
  unchanged : Bool
  entries : List Entry
  deriving ToJson

@[conformance_family]
def observation : Family Input Output where
  inputs :=
    [ ⟨["pending", "unobserved"], ["pending", "later"], 0, 3, .duringProbe⟩
    , ⟨["unobserved"], ["pending"], 0, 3, .beforeCycle⟩
    , ⟨[], ["pending"], 0, 3, .beforeCycle⟩
    , ⟨["pending", "unobserved"], ["pending", "later"], 1, 3, .duringProbe⟩
    , ⟨["unobserved"], ["first", "second", "third"], 2, 1, .duringProbe⟩
    , ⟨[], ["pending"], 0, 3, .duringProbe⟩ ]
  model := fun i =>
    let K : Threshold := ⟨max 1 i.threshold, Nat.le_max_left 1 _⟩
    let before : HealthMap := i.known.map fun id => (id, ServiceModel.initial)
    let (probes, after) := cycle K before i.registry i.failing i.shutdown
    { probesStarted := probes
      unchanged := sameEntries before after
      entries := (after.map fun e =>
          ({ service := e.1, state := e.2.state.toDefraDB, failureCount := e.2.failureCount } : Entry)
        ).mergeSort fun a b => !(decide (b.service < a.service)) }

end Conformance.MCPHealthShutdown

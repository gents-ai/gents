import Proofs.Tools.ToolPolicy.PluginPublication
import Proofs.Conformance.Family

namespace Conformance.PluginPublication

open Lean ToolPolicy.PluginPublication

structure RetryInput where
  busy : Bool
  now : Nat
  deadline : Nat
  deriving ToJson

structure NextAttemptInput where
  now : Nat
  deadline : Nat
  poll : Nat
  deriving ToJson

@[conformance_family]
def retry : Family RetryInput Bool where
  inputs := [false, true].flatMap fun busy =>
    [(0, 0), (0, 100), (99, 100), (100, 100), (250, 100)].map fun (now, deadline) =>
      { busy, now, deadline }
  model := fun i => ToolPolicy.PluginPublication.retry i.busy i.now i.deadline

@[conformance_family]
def nextAttempt : Family NextAttemptInput Nat where
  inputs :=
    [ { now := 0, deadline := 60000, poll := 100 }
    , { now := 59950, deadline := 60000, poll := 100 }
    , { now := 60000, deadline := 60000, poll := 100 }
    , { now := 0, deadline := 50, poll := 100 } ]
  model := fun i => ToolPolicy.PluginPublication.nextAttempt i.now i.deadline i.poll

end Conformance.PluginPublication

import Proofs.Runtime.ReadinessPersistence
import Proofs.Conformance.Family

namespace Conformance.StartupPublication

open Lean Runtime.ReadinessPersistence

structure Output where
  failure : Option StartupCommand
  attempts : Nat
  readyPersisted : Bool
  shutdownPersisted : Bool
  readyObserved : Bool
  deriving ToJson

@[conformance_family]
def startup : Family StartupRun Output where
  inputs :=
    [ ⟨["ok"], ["ok"]⟩
    , ⟨List.replicate 5 "fail", ["ok"]⟩
    , ⟨List.replicate 4 "fail" ++ ["ok"], ["ok"]⟩
    , ⟨["ok"], List.replicate 5 "fail"⟩
    , ⟨["ok"], ["fail", "ok"]⟩ ]
  model := fun i =>
    { failure := startupFailure i
      attempts := startupAttempts i
      readyPersisted := "ready" ∈ startupPersisted i
      shutdownPersisted := "shutdown" ∈ startupPersisted i
      readyObserved := startupReadyObserved i }

end Conformance.StartupPublication

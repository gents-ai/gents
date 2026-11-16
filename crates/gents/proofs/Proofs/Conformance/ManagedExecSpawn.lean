import Proofs.Session.ManagedExec
import Proofs.Conformance.Family

namespace Conformance.ManagedExecSpawn

open Lean

structure SpawnInput where
  spawnFails : Bool
  deriving ToJson

structure SpawnOutput where
  state : String
  executorRegistered : Bool
  deriving ToJson

@[conformance_family]
def outcome : Family SpawnInput SpawnOutput where
  inputs := [{ spawnFails := true }]
  model := fun i =>
    { state := (ManagedExecContext.spawnAttemptState i.spawnFails).toDefraDB
      executorRegistered :=
        ManagedExecContext.registersExecutor (ManagedExecContext.spawnAttemptState i.spawnFails) }

end Conformance.ManagedExecSpawn

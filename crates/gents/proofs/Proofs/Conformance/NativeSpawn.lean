import Proofs.NativeSpawn
import Lean

/-! Generated native spawn cases: every platform and first/retry outcome pair,
with the attempted paths and caller result derived by `NativeSpawn.run`. -/

namespace Conformance.NativeSpawn
open Lean _root_.NativeSpawn

private def platformName : Platform → String
  | .macos => "macos"
  | .other => "other"

private def outcomeName : Outcome → String
  | .ok => "ok"
  | .err .badDescriptor => "ebadf"
  | .err .other => "other"

private def pathName : SpawnPath → String
  | .posixSpawn => "posix_spawn"
  | .forkExec => "fork_exec"

private def outcomes : List Outcome := [.ok, .err .badDescriptor, .err .other]

private def caseJson (platform : Platform) (first retry : Outcome) : Json :=
  let (paths, result) := run platform first retry
  Json.mkObj [("platform", toJson (platformName platform)),
    ("first", toJson (outcomeName first)), ("retry", toJson (outcomeName retry)),
    ("paths", toJson (paths.map pathName)), ("result", toJson (outcomeName result))]

def casesJson : String :=
  (toJson ([Platform.macos, .other].flatMap fun platform =>
    outcomes.flatMap fun first => outcomes.map (caseJson platform first))).compress

end Conformance.NativeSpawn

def main : IO Unit := IO.println Conformance.NativeSpawn.casesJson

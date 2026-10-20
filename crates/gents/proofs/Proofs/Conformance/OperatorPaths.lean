import Proofs.ToolPolicy.OperatorPaths
import Lean

namespace Conformance.OperatorPaths
open Lean

private def pathCases : List Json :=
  ([ ("ordinary", false, some false), ("ordinaryAlias", false, some false),
     ("writeOnly", false, some false),
     ("missing", false, some false), ("named", true, some true),
     ("uncreatedNamed", true, some false), ("alias", false, some true),
     ("queueAlias", false, some true),
     ("staged", true, some true), ("stagedAlias", false, some true),
     ("uncreatedStaged", true, some false), ("unavailable", false, none),
     ("guardUnavailable", false, none) ] : List (String × Bool × Option Bool)).map fun (kind, named, object) =>
    Json.mkObj [("kind", toJson kind),
      ("expected", toJson (ToolPolicy.OperatorPaths.isProtected named object))]

private def commandCases : List Json :=
  [false, true].flatMap fun required =>
  [false, true].flatMap fun supported =>
  [false, true].flatMap fun inspected =>
  [false, true].map fun linked => Json.mkObj [
    ("kind", toJson "commandGuard"),
    ("command", Json.mkObj [("required", toJson required),
      ("supported", toJson supported), ("inspected", toJson inspected),
      ("singleLinked", toJson linked)]),
    ("expected", toJson (ToolPolicy.OperatorPaths.commandGuardAllows required supported inspected linked))]

def casesJson : Json := toJson (pathCases ++ commandCases)

end Conformance.OperatorPaths

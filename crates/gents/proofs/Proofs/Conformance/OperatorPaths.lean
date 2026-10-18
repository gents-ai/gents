import Proofs.ToolPolicy.OperatorPaths
import Lean

namespace Conformance.OperatorPaths
open Lean

def casesJson : Json := toJson <|
  ([ ("ordinary", false, some false), ("ordinaryAlias", false, some false),
     ("writeOnly", false, some false),
     ("missing", false, some false), ("named", true, some true),
     ("uncreatedNamed", true, some false), ("alias", false, some true),
     ("queueAlias", false, some true), ("unavailable", false, none),
     ("guardUnavailable", false, none) ] : List (String × Bool × Option Bool)).map fun (kind, named, object) =>
    Json.mkObj [("kind", toJson kind),
      ("expected", toJson (ToolPolicy.OperatorPaths.isProtected named object))]

end Conformance.OperatorPaths

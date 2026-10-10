import Lean
import Proofs.ToolPolicy.PluginReceipt

namespace Conformance.PluginReceipt
open Lean

def casesJson : Json := toJson <| [false, true].flatMap fun generationMatches =>
  [false, true].flatMap fun terminal =>
  [none, some "receipt", some "different"].map fun existing => Json.mkObj [
    ("generation_matches", toJson generationMatches),
    ("terminal", toJson terminal),
    ("existing", toJson existing),
    ("receipt", toJson "receipt"),
    ("accepted", toJson (ToolPolicy.PluginReceipt.mayCommit
      generationMatches terminal existing "receipt"))]

end Conformance.PluginReceipt

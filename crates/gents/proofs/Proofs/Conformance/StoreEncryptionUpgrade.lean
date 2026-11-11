import Proofs.Store.StoreEncryptionUpgrade
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.StoreEncryptionUpgrade
open _root_.StoreEncryptionUpgrade Conformance.Contracts

private def phaseName : Phase → String
  | .copying => "copying" | .verified => "verified" | .installed => "installed"
private def actionName : Action → String
  | .copy => "copy" | .retireSource => "retire_source" | .promote => "promote"
  | .recordInstalled => "record_installed" | .open => "open" | .refuse => "refuse"
private def boolean (b : Bool) : String := if b then "true" else "false"

def casesJson : String := jsonArray <| do
  let p ← [Phase.copying, .verified, .installed]
  let source ← [false, true]
  let stage ← [false, true]
  let retired ← [false, true]
  let equal ← [false, true]
  let metadata ← [false, true]
  let accepted ← [false, true]
  pure ("{\"phase\":" ++ jsonString (phaseName p) ++
    ",\"source\":" ++ boolean source ++ ",\"stage\":" ++ boolean stage ++
    ",\"retired\":" ++ boolean retired ++ ",\"action\":" ++
    jsonString (actionName (next p source stage retired)) ++
    ",\"equal\":" ++ boolean equal ++ ",\"rechecked_phase\":" ++
    jsonString (phaseName (afterRecheck equal)) ++
    ",\"metadata\":" ++ boolean metadata ++ ",\"accepted\":" ++ boolean accepted ++
    ",\"may_finish\":" ++ boolean (mayFinish p metadata accepted) ++ "}")
end Conformance.StoreEncryptionUpgrade

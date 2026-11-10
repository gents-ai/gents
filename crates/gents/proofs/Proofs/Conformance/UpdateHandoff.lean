import Proofs.RuntimeReconcile.UpdateHandoff
import Lean
namespace Conformance.UpdateHandoff
open Lean _root_.RuntimeReconcile.UpdateHandoff
private def phaseName : Phase → String
  | .staged => "staged" | .registered => "registered" | .accepted => "accepted"
  | .retired => "retired" | .released => "released" | .backedUp => "backed_up"
  | .installed => "installed" | .healthy => "healthy" | .failed => "failed"
private def actions : List (String × Bool × Action) :=
  [false, true].flatMap fun flag =>
    [("register", flag, .register flag), ("retire", flag, .retire flag),
     ("release", flag, .release flag), ("backup", flag, .backup flag),
     ("install", flag, .install flag), ("verify", flag, .verify flag)] ++
  [("acknowledge", false, .acknowledge), ("fail", false, .fail)]
def casesJson : String :=
  let phases := [Phase.staged, .registered, .accepted, .retired, .released,
    .backedUp, .installed, .healthy, .failed]
  (toJson (phases.flatMap fun phase => actions.map fun (name, flag, action) =>
    Json.mkObj [("phase", toJson (phaseName phase)), ("action", toJson name),
      ("flag", toJson flag), ("can_restore", toJson (canRestore phase flag false)), ("can_restore_same_build", toJson (canRestore phase flag true)), ("next", match step phase action with
        | some next => toJson (phaseName next) | none => Json.null)])).compress
end Conformance.UpdateHandoff

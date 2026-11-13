import Proofs.Tools.ToolPolicy.WorkspacePermissions
import Proofs.Conformance.Family

namespace Conformance.WorkspaceFileConsent
open Lean

structure ConsentInput where
  mode : Option String
  inside : Bool
  dryRun : Bool
  deriving ToJson

structure ConsentOutput where
  asked : Bool
  completed : Bool
  changed : Bool
  deriving ToJson

private def modeOf (wire : String) : ToolPolicy.WorkspacePermissions.Mode :=
  (([.ask, .autoReview, .fullAccess] : List ToolPolicy.WorkspacePermissions.Mode).find?
    (·.wireName == wire)).getD .ask

private def savedOf (i : ConsentInput) : Option ToolPolicy.WorkspacePermissions.Input :=
  i.mode.map fun wire =>
    { current := { workspace := 1, revision := 2 }
      configuredWorkspace := 1
      mode := modeOf wire
      action := 3
      ceilingAllows := true
      crossesBoundary := !i.inside
      grant := none }

@[conformance_family]
def fileTools : Family ConsentInput ConsentOutput where
  inputs :=
    ([none, some "ask", some "fullAccess"] : List (Option String)).flatMap fun mode =>
    [false, true].flatMap fun inside =>
    [false, true].map fun dryRun => { mode, inside, dryRun }
  model := fun i =>
    let route := ToolPolicy.WorkspacePermissions.fileToolRoute (savedOf i) i.dryRun
    { asked := decide (route = .ask)
      completed := decide (route = .allow)
      changed := decide (route = .allow) && !i.dryRun }

end Conformance.WorkspaceFileConsent

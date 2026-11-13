import Proofs.Tools.ToolPolicy.WorkspacePermissions
import Proofs.Conformance.Family

namespace Conformance.WorkspacePermissions

open Lean ToolPolicy.WorkspacePermissions

deriving instance ToJson for Mode

structure ConsentInput where
  preference : Option Mode
  readOnly : Bool
  interactive : Bool
  deriving ToJson

structure ConsentOutput where
  runs : Bool
  asksPerson : Bool
  deriving ToJson

def consentSituations : List ConsentInput :=
  [none, some Mode.ask, some .autoReview, some .fullAccess].flatMap fun preference =>
    [false, true].flatMap fun readOnly =>
      [false, true].map fun interactive => { preference, readOnly, interactive }

/-- A native bash command run under a workspace scope with no responder and no
reviewer. An interactive question waits for a person who never answers here, so
the interactive situations are those the model never routes to a person. -/
@[conformance_family]
def commandConsent : Family ConsentInput ConsentOutput where
  inputs := consentSituations.filter fun i =>
    !i.interactive || !needsPerson i.preference i.readOnly
  model := fun i =>
    { runs := commandRuns i.preference i.readOnly
      asksPerson := asksPerson i.preference i.readOnly i.interactive }

end Conformance.WorkspacePermissions

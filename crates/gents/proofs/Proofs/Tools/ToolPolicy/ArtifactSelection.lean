import Proofs.Session.CommandPolicy.ArtifactAuthority

/-! The static command mode a tool selection composes with the operator
ceiling. A bash selection that is not unrestricted runs read-only whatever the
authored execution mode is; a read-only operator ceiling clamps the result
again. An artifact-write mode therefore survives only when the selection is
unrestricted and the ceiling is not read-only. -/

namespace ToolPolicy.ArtifactSelection

open CommandPolicy

def selectedMode (bashUnrestricted : Bool) (authored : ExecutionMode) : ExecutionMode :=
  if bashUnrestricted then authored else .readOnly

def ceilingMode (readOnlyCeiling : Bool) : ExecutionMode :=
  if readOnlyCeiling then .readOnly else .unrestricted

def staticMode (bashUnrestricted readOnlyCeiling : Bool) (authored : ExecutionMode) :
    ExecutionMode :=
  (selectedMode bashUnrestricted authored).meet (ceilingMode readOnlyCeiling)

def netDisabled : NetworkMode → Bool
  | .disabled => true
  | _ => false

def ceilingNetwork : NetworkMode := .enabled

def staticNetworkDisabled (authored : NetworkMode) : Bool :=
  netDisabled authored || netDisabled ceilingNetwork

theorem artifact_selection_preserves_effect_only_after_bash_and_operator_ceilings
    (bashUnrestricted readOnlyCeiling : Bool) :
    staticMode bashUnrestricted readOnlyCeiling .artifactWrite = .artifactWrite ↔
      bashUnrestricted = true ∧ readOnlyCeiling = false := by
  cases bashUnrestricted <;> cases readOnlyCeiling <;> decide

end ToolPolicy.ArtifactSelection

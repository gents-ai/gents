import Proofs.CommandPolicy.Types

namespace CommandPolicy

/-- A selected project may narrow a sandboxed command's writable root but
cannot widen its configured tool ceiling. Canonical host containment supplies
the path relation. Full access retains that ceiling; unrestricted commands and
artifact grants retain their existing execution owners. -/
def useWorkspaceRoot (mode : ExecutionMode) (configured fullAccess withinToolRoot : Bool) : Bool :=
  mode == .workspaceWrite && configured && !fullAccess && withinToolRoot

theorem unconfigured_workspace_preserves_tool_root (mode : ExecutionMode) (full within : Bool) :
    useWorkspaceRoot mode false full within = false := by simp [useWorkspaceRoot]

theorem workspace_root_cannot_widen_tool_root (mode : ExecutionMode) (configured full : Bool) :
    useWorkspaceRoot mode configured full false = false := by simp [useWorkspaceRoot]

theorem full_access_preserves_tool_root (mode : ExecutionMode) (configured within : Bool) :
    useWorkspaceRoot mode configured true within = false := by simp [useWorkspaceRoot]

theorem unrestricted_preserves_execution_root (configured full within : Bool) :
    useWorkspaceRoot .unrestricted configured full within = false := by simp [useWorkspaceRoot]

def selectSandbox
    (support : RuntimeSupport)
    (mode : ExecutionMode) : SandboxDecision :=
  match mode with
  | .readOnly => .selected .policyReadOnly
  | .unrestricted => .selected .unsandboxedUnrestricted
  | .workspaceWrite =>
      if support.workspaceWriteSandboxEnforced then
        .selected .macosSeatbelt
      else
        .denied .workspaceWriteSandboxUnavailable

  | .artifactWrite =>
      if support.workspaceWriteSandboxEnforced then
        .selected .macosSeatbelt
      else
        .denied .artifactWriteSandboxUnavailable

end CommandPolicy

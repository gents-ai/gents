import Proofs.CommandPolicy.Types

namespace CommandPolicy

/-- A bounded command inherits the sandbox's offline default, not the host's
network authority. Explicit network configuration still meets the existing tool
ceiling before launch. Unrestricted and argv-validated read-only commands retain
their own enforcement owners. -/
def effectiveNetworkMode (mode : ExecutionMode) (network : NetworkMode) : NetworkMode :=
  match mode, network with
  | .workspaceWrite, .inherit | .artifactWrite, .inherit => .disabled
  | _, network => network

/-- The native adapter supplies the credential namespace from host-owned home
and identity configuration. Seatbelt must exclude both its names and resolved
targets, independently of optional workspace consent. Filesystem namespace
replacement by an outside host process after inspection is an external premise;
the child may not move protected ancestors or create hard-link aliases. -/
def protectsCredentials (mode : ExecutionMode) : Bool :=
  mode == .workspaceWrite || mode == .artifactWrite

theorem bounded_inherits_no_network :
    effectiveNetworkMode .workspaceWrite .inherit = .disabled ∧
    effectiveNetworkMode .artifactWrite .inherit = .disabled := by decide

theorem disabled_network_is_never_enabled (mode : ExecutionMode) :
    effectiveNetworkMode mode .disabled = .disabled := by cases mode <;> rfl

theorem explicit_network_preserved (mode : ExecutionMode) :
    effectiveNetworkMode mode .enabled = .enabled := by cases mode <;> rfl

theorem bounded_credentials_are_protected :
    protectsCredentials .workspaceWrite = true ∧
    protectsCredentials .artifactWrite = true := by decide

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

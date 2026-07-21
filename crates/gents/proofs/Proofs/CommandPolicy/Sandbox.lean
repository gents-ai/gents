import Proofs.CommandPolicy.Types

/-!
# Command Policy Sandbox Selection

Model of the sandbox label selected before command execution.
-/

namespace CommandPolicy

/-- Select the sandbox envelope required by the execution mode. -/
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

end CommandPolicy

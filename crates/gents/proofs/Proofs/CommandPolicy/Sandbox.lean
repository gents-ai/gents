import Proofs.CommandPolicy.Types

namespace CommandPolicy

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

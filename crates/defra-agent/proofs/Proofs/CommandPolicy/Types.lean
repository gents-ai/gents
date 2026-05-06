import Proofs.Basic

/-!
# Command Policy Types

Pure Lean vocabulary for the command/tool execution policy enforced by
`toolset/shared/command.rs` and surfaced through desired-state tool selections.
-/

namespace CommandPolicy

/-- Mirrors `CommandExecutionMode`. -/
inductive ExecutionMode where
  | readOnly
  | workspaceWrite
  | unrestricted
  deriving DecidableEq, Repr

/-- Mirrors `CommandNetworkMode`. -/
inductive NetworkMode where
  | inherit
  | disabled
  | enabled
  deriving DecidableEq, Repr

/-- A command request after bash tool argument normalization. -/
structure CommandRequest where
  command : String
  args : List String
  deriving DecidableEq, Repr

namespace CommandRequest

/-- The argv vector checked against allowed/forbidden prefixes. -/
def argv (request : CommandRequest) : List String :=
  request.command :: request.args

end CommandRequest

/-- Reasons the policy validator can deny a command. -/
inductive DenialReason where
  | forbiddenPrefix (matched : List String)
  | allowedPrefixRequired (argv : List String)
  | readOnlyCommandNotAllowlisted (command : String)
  | disabledNetworkUnenforceable
  | disabledNetworkCommand (command : String)
  | workspaceWriteSandboxUnavailable
  deriving DecidableEq, Repr

/-- Executable validator result. -/
inductive Decision where
  | allow
  | deny (reason : DenialReason)
  deriving DecidableEq, Repr

/-- Command execution policy as configured from a tool selection. -/
structure Policy where
  mode : ExecutionMode
  allowedArgvPrefixes : List (List String)
  forbiddenArgvPrefixes : List (List String)
  networkMode : NetworkMode
  readOnlyAllowlist : List String
  deriving DecidableEq, Repr

/-- Runtime sandbox labels emitted in command metadata. -/
inductive SandboxKind where
  | policyReadOnly
  | macosSeatbelt
  | unsandboxedUnrestricted
  deriving DecidableEq, Repr

/-- Sandbox selection may fail before spawning the process. -/
inductive SandboxDecision where
  | selected (sandbox : SandboxKind)
  | denied (reason : DenialReason)
  deriving DecidableEq, Repr

/-- Host capabilities relevant to workspace-write sandbox enforcement. -/
structure RuntimeSupport where
  workspaceWriteSandboxEnforced : Bool
  deriving DecidableEq, Repr

end CommandPolicy

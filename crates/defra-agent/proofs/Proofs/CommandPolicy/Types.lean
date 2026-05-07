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

namespace ExecutionMode

def toDefraDB : ExecutionMode → String
  | .readOnly => "read_only"
  | .workspaceWrite => "workspace_write"
  | .unrestricted => "unrestricted"

end ExecutionMode

/-- Mirrors `CommandNetworkMode`. -/
inductive NetworkMode where
  | inherit
  | disabled
  | enabled
  deriving DecidableEq, Repr

namespace NetworkMode

def toDefraDB : NetworkMode → String
  | .inherit => "inherit"
  | .disabled => "disabled"
  | .enabled => "enabled"

end NetworkMode

/-- A command request after bash tool argument normalization.

`command` is the raw argv[0] used by allowed/forbidden argv-prefix checks.
`lookupCommand` is the Rust `executable_name_lookup_key` result used by
read-only command and network gates: the executable basename when available,
otherwise the raw command string. -/
structure CommandRequest where
  command : String
  lookupCommand : String
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
  | readOnlyArgumentNotAllowed (command : String) (argument : String)
  | readOnlySubcommandRequired (command : String)
  | readOnlySubcommandNotAllowlisted (command : String) (subcommand : String)
  | readOnlyUrlRequired (command : String)
  | disabledNetworkUnenforceable
  | disabledNetworkCommand (command : String)
  | workspaceWriteSandboxUnavailable
  deriving DecidableEq, Repr

namespace DenialReason

def toContract : DenialReason → String
  | .forbiddenPrefix _ => "forbiddenPrefix"
  | .allowedPrefixRequired _ => "allowedPrefixRequired"
  | .readOnlyCommandNotAllowlisted _ => "readOnlyCommandNotAllowlisted"
  | .readOnlyArgumentNotAllowed _ _ => "readOnlyArgumentNotAllowed"
  | .readOnlySubcommandRequired _ => "readOnlySubcommandRequired"
  | .readOnlySubcommandNotAllowlisted _ _ => "readOnlySubcommandNotAllowlisted"
  | .readOnlyUrlRequired _ => "readOnlyUrlRequired"
  | .disabledNetworkUnenforceable => "disabledNetworkUnenforceable"
  | .disabledNetworkCommand _ => "disabledNetworkCommand"
  | .workspaceWriteSandboxUnavailable => "workspaceWriteSandboxUnavailable"

def matchedPrefix? : DenialReason → Option (List String)
  | .forbiddenPrefix matched => some matched
  | _ => none

def argv? : DenialReason → Option (List String)
  | .allowedPrefixRequired argv => some argv
  | _ => none

def command? : DenialReason → Option String
  | .readOnlyCommandNotAllowlisted command => some command
  | .readOnlyArgumentNotAllowed command _ => some command
  | .readOnlySubcommandRequired command => some command
  | .readOnlySubcommandNotAllowlisted command _ => some command
  | .readOnlyUrlRequired command => some command
  | .disabledNetworkCommand command => some command
  | _ => none

def argument? : DenialReason → Option String
  | .readOnlyArgumentNotAllowed _ argument => some argument
  | _ => none

def subcommand? : DenialReason → Option String
  | .readOnlySubcommandNotAllowlisted _ subcommand => some subcommand
  | _ => none

end DenialReason

/-- Executable validator result. -/
inductive Decision where
  | allow
  | deny (reason : DenialReason)
  deriving DecidableEq, Repr

namespace Decision

def toContract : Decision → String
  | .allow => "allow"
  | .deny _ => "deny"

def denialReason? : Decision → Option DenialReason
  | .allow => none
  | .deny reason => some reason

end Decision

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

namespace SandboxKind

def toContract : SandboxKind → String
  | .policyReadOnly => "policy_read_only"
  | .macosSeatbelt => "macos_seatbelt"
  | .unsandboxedUnrestricted => "unsandboxed_unrestricted"

end SandboxKind

/-- Sandbox selection may fail before spawning the process. -/
inductive SandboxDecision where
  | selected (sandbox : SandboxKind)
  | denied (reason : DenialReason)
  deriving DecidableEq, Repr

namespace SandboxDecision

def toContract : SandboxDecision → String
  | .selected _ => "selected"
  | .denied _ => "denied"

def sandbox? : SandboxDecision → Option SandboxKind
  | .selected sandbox => some sandbox
  | .denied _ => none

def denialReason? : SandboxDecision → Option DenialReason
  | .selected _ => none
  | .denied reason => some reason

end SandboxDecision

/-- Host capabilities relevant to workspace-write sandbox enforcement. -/
structure RuntimeSupport where
  workspaceWriteSandboxEnforced : Bool
  deriving DecidableEq, Repr

end CommandPolicy

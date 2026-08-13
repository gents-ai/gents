import Proofs.Basic

namespace CommandPolicy

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

structure CommandRequest where
  command : String
  lookupCommand : String
  args : List String
  deriving DecidableEq, Repr

namespace CommandRequest

def argv (request : CommandRequest) : List String :=
  request.command :: request.args

end CommandRequest

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
  | workspaceExecutable
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
  | .workspaceExecutable => "workspaceExecutable"

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

structure Policy where
  mode : ExecutionMode
  allowedArgvPrefixes : List (List String)
  forbiddenArgvPrefixes : List (List String)
  networkMode : NetworkMode
  readOnlyAllowlist : List String
  deriving DecidableEq, Repr

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

structure RuntimeSupport where
  workspaceWriteSandboxEnforced : Bool
  deriving DecidableEq, Repr

end CommandPolicy

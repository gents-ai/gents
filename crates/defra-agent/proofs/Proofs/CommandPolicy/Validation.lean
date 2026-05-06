import Proofs.CommandPolicy.Types

/-!
# Command Policy Validation

Executable validation model for argv prefix filters, read-only command
allowlisting, and disabled-network behavior.
-/

namespace CommandPolicy

/-- Whether `candidate` matches the beginning of `argv`. -/
def matchesPrefix : List String → List String → Bool
  | [], _ => true
  | _ :: _, [] => false
  | p :: ps, a :: rest =>
      if p = a then matchesPrefix ps rest else false

/-- First configured argv prefix that matches `argv`, preserving Rust's list order. -/
def firstMatchingPrefix (argv : List String) : List (List String) → Option (List String)
  | [] => none
  | candidate :: rest =>
      if matchesPrefix candidate argv then
        some candidate
      else
        firstMatchingPrefix argv rest

/-- Read-only command allowlist check. The Rust implementation also normalizes
    executable paths to file names; this model captures the allowlist gate. -/
def commandAllowlisted (command : String) (allowlist : List String) : Bool :=
  allowlist.any (fun allowed => decide (allowed = command))

/-- Validator used when the effective mode is `read_only`. -/
def validateReadOnlyCommand
    (allowlist : List String)
    (request : CommandRequest) : Decision :=
  if commandAllowlisted request.command allowlist then
    .allow
  else
    .deny (.readOnlyCommandNotAllowlisted request.command)

/-- Read-only commands that cannot be allowed when network is disabled without
    a sandbox-level network denial. -/
def readOnlyNetworkDenied (request : CommandRequest) : Bool :=
  if request.command = "curl" then
    true
  else if request.command = "tailscale" then
    match request.args with
    | "ping" :: _ => true
    | "netcheck" :: _ => true
    | _ => false
  else
    false

/-- Network-mode gate. `workspace_write` can enforce disabled networking through
    its sandbox; `unrestricted` cannot, so it fails closed. -/
def validateNetworkMode
    (mode : ExecutionMode)
    (networkMode : NetworkMode)
    (request : CommandRequest) : Decision :=
  match networkMode with
  | .inherit => .allow
  | .enabled => .allow
  | .disabled =>
      match mode with
      | .workspaceWrite => .allow
      | .unrestricted => .deny .disabledNetworkUnenforceable
      | .readOnly =>
          if readOnlyNetworkDenied request then
            .deny (.disabledNetworkCommand request.command)
          else
            .allow

/-- Validation once argv prefix checks have passed. -/
def validateAfterPrefixes (policy : Policy) (request : CommandRequest) : Decision :=
  match validateNetworkMode policy.mode policy.networkMode request with
  | .deny reason => .deny reason
  | .allow =>
      match policy.mode with
      | .readOnly => validateReadOnlyCommand policy.readOnlyAllowlist request
      | .workspaceWrite => .allow
      | .unrestricted => .allow

/-- Policy validation order: forbidden prefixes, allowed-prefix list, network
    gate, and finally the read-only allowlist. -/
def validatePolicy (policy : Policy) (request : CommandRequest) : Decision :=
  match firstMatchingPrefix request.argv policy.forbiddenArgvPrefixes with
  | some matched => .deny (.forbiddenPrefix matched)
  | none =>
      match policy.allowedArgvPrefixes with
      | [] => validateAfterPrefixes policy request
      | _ :: _ =>
          match firstMatchingPrefix request.argv policy.allowedArgvPrefixes with
          | none => .deny (.allowedPrefixRequired request.argv)
          | some _ => validateAfterPrefixes policy request

end CommandPolicy

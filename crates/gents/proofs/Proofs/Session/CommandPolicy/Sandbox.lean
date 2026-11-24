import Proofs.Session.CommandPolicy.Types

namespace CommandPolicy

/-- A bounded command inherits the sandbox's offline default, not the host's
network authority. Explicit network configuration still meets the existing tool
ceiling before launch. A read-only command's kernel network is `kernelNetwork`;
an unrestricted command has no kernel owner. -/
def effectiveNetworkMode (mode : ExecutionMode) (network : NetworkMode) : NetworkMode :=
  match mode, network with
  | .workspaceWrite, .inherit | .artifactWrite, .inherit => .disabled
  | _, network => network

/-- The native adapter supplies the credential namespace from host-owned home
and identity configuration. Every Seatbelt profile a mode selects excludes both
its names and resolved targets, independently of optional workspace consent:
the read-only profile reads everywhere else, so it excludes them as the
workspace and artifact profiles do. A read-only launch no profile contains (a
program in `seatbeltRefusedPrograms`, a host with no sandbox backend) has no
kernel to enforce the exclusion (`contained_commands_protect_credentials`).
Filesystem namespace replacement by an outside host process after inspection is
an external premise; the child may not move protected ancestors or create
hard-link aliases. -/
def protectsCredentials (mode : ExecutionMode) : Bool :=
  mode != .unrestricted

theorem bounded_inherits_no_network :
    effectiveNetworkMode .workspaceWrite .inherit = .disabled ∧
    effectiveNetworkMode .artifactWrite .inherit = .disabled := by decide

theorem disabled_network_is_never_enabled (mode : ExecutionMode) :
    effectiveNetworkMode mode .disabled = .disabled := by cases mode <;> rfl

theorem explicit_network_preserved (mode : ExecutionMode) :
    effectiveNetworkMode mode .enabled = .enabled := by cases mode <;> rfl

theorem contained_modes_protect_credentials :
    protectsCredentials .readOnly = true ∧
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

/-- Host programs a Seatbelt profile cannot run: `ps` and `sudo` are setuid
root, and the kernel refuses to execute a setuid program inside a sandbox; launchd
answers `launchctl list` with an error to any sandboxed caller. A read-only
request whose resolved program is one of them runs outside the profile, bounded
by its argument validator: `ps` has no option that writes or starts a program,
`sudo` accepts only `/bin/launchctl`, and `launchctl` accepts only `list`,
`print`, `print-disabled` and `blame`, which ask launchd and start nothing.
A process the profile contains cannot execute `ps` or `sudo` at all. -/
def seatbeltRefusedPrograms : List String :=
  ["/bin/ps", "/usr/bin/sudo", "/bin/launchctl"]

def selectSandbox
    (support : RuntimeSupport)
    (mode : ExecutionMode)
    (program : String) : SandboxDecision :=
  match mode with
  | .readOnly =>
      if support.workspaceWriteSandboxEnforced && !seatbeltRefusedPrograms.contains program then
        .selected .macosSeatbeltReadOnly
      else
        .selected .policyReadOnly
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

/-- A read-only command reads the workspace and the host; reaching the network
is a read only for the commands whose purpose it is: `curl` fetches a URL and
every `tailscale` subcommand asks the local daemon over its socket. -/
def readOnlyReachesNetwork (request : CommandRequest) : Bool :=
  ["curl", "tailscale"].contains request.lookupCommand

def NetworkMode.allowsNetwork : NetworkMode → Bool
  | .disabled => false
  | .inherit | .enabled => true

/-- Whether the kernel lets the command open a network connection. A bounded
command follows its effective network mode; a read-only command reaches the
network only when it is a network reader and the policy has not disabled the
network. -/
def kernelNetwork (mode : ExecutionMode) (network : NetworkMode) (reachesNetwork : Bool) : Bool :=
  match mode with
  | .readOnly => reachesNetwork && (effectiveNetworkMode mode network).allowsNetwork
  | _ => (effectiveNetworkMode mode network).allowsNetwork

/-- The Seatbelt profile each sandbox applies. A workspace or artifact command
writes only under its writable root and sees only its own processes. A
read-only command writes nothing: no file is created, written, renamed, linked,
unlinked or changed in mode, owner or time anywhere, and the only file it may
open for writing is `/dev/null`; it may inspect every process, which `lsof` and
other diagnostics need, but signal only its own. -/
def seatbeltProfile (sandbox : SandboxKind) (network : Bool) : Option SeatbeltProfile :=
  match sandbox with
  | .macosSeatbelt => some { writes := .writableRoot, network, processInfo := .sameSandbox }
  | .macosSeatbeltReadOnly => some { writes := .nothing, network, processInfo := .all }
  | .policyReadOnly | .unsandboxedUnrestricted => none

/-- The kernel profile a command runs under: the selected sandbox's profile with
its network decision, or none when no profile contains it. -/
def commandProfile
    (support : RuntimeSupport)
    (policy : Policy)
    (request : CommandRequest)
    (program : String) : Option SeatbeltProfile :=
  match selectSandbox support policy.mode program with
  | .selected sandbox =>
      seatbeltProfile sandbox
        (kernelNetwork policy.mode policy.networkMode (readOnlyReachesNetwork request))
  | .denied _ => none

/-- Whatever profile contains a command excludes the credential namespace. -/
theorem contained_commands_protect_credentials
    (support : RuntimeSupport)
    (policy : Policy)
    (request : CommandRequest)
    (program : String)
    (profile : SeatbeltProfile)
    (contained : commandProfile support policy request program = some profile) :
    protectsCredentials policy.mode = true := by
  cases h : policy.mode with
  | unrestricted => simp [commandProfile, selectSandbox, h, seatbeltProfile] at contained
  | readOnly => decide
  | workspaceWrite => decide
  | artifactWrite => decide

end CommandPolicy

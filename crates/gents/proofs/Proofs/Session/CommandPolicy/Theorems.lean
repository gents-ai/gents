import Proofs.Session.CommandPolicy.Validation
import Proofs.Session.CommandPolicy.Sandbox
import Proofs.Session.CommandPolicy.Env
import Proofs.Session.CommandPolicy.Launch

namespace CommandPolicy

theorem forbidden_prefixes_deny
    (policy : Policy)
    (request : CommandRequest)
    (matched : List String)
    (hmatch :
      firstMatchingPrefix request.argv policy.forbiddenArgvPrefixes = some matched) :
    validatePolicy policy request = .deny (.forbiddenPrefix matched) := by
  simp [validatePolicy, hmatch]

theorem allowed_prefixes_deny_unmatched
    (policy : Policy)
    (request : CommandRequest)
    (first : List String)
    (rest : List (List String))
    (hconfigured : policy.allowedArgvPrefixes = first :: rest)
    (hforbidden :
      firstMatchingPrefix request.argv policy.forbiddenArgvPrefixes = none)
    (hunmatched :
      firstMatchingPrefix request.argv policy.allowedArgvPrefixes = none) :
    validatePolicy policy request = .deny (.allowedPrefixRequired request.argv) := by
  have hunmatched' :
      firstMatchingPrefix request.argv (first :: rest) = none := by
    simpa [hconfigured] using hunmatched
  simp [validatePolicy, hforbidden, hconfigured, hunmatched']

theorem readOnly_validator_rejects_unallowlisted
    (allowlist : List String)
    (request : CommandRequest)
    (hunallowlisted :
      commandAllowlisted request.lookupCommand allowlist = false) :
    validateReadOnlyCommand allowlist false request =
      .deny (.readOnlyCommandNotAllowlisted request.lookupCommand) := by
  simp [validateReadOnlyCommand, hunallowlisted]

theorem readOnly_validator_allows_only_allowlisted_or_configured_prefix
    (allowlist : List String)
    (allowedPrefixMatched : Bool)
    (request : CommandRequest)
    (hallow : validateReadOnlyCommand allowlist allowedPrefixMatched request = .allow) :
    commandAllowlisted request.lookupCommand allowlist = true
      ∨ allowedPrefixMatched = true := by
  cases hlisted : commandAllowlisted request.lookupCommand allowlist with
  | true => exact Or.inl rfl
  | false =>
      cases hprefix : allowedPrefixMatched with
      | true => exact Or.inr rfl
      | false =>
          simp [validateReadOnlyCommand, hlisted, hprefix] at hallow

theorem disabled_network_unrestricted_fails_closed
    (request : CommandRequest) :
    validateNetworkMode .unrestricted .disabled request =
      .deny .disabledNetworkUnenforceable := by
  rfl

theorem disabled_network_readOnly_curl_denies
    (args : List String) :
    validateNetworkMode .readOnly .disabled
        { command := "curl", lookupCommand := "curl", args := args } =
      .deny (.disabledNetworkCommand "curl") := by
  rfl

theorem disabled_network_readOnly_lookup_curl_denies
    (command : String)
    (args : List String) :
    validateNetworkMode .readOnly .disabled
        { command := command, lookupCommand := "curl", args := args } =
      .deny (.disabledNetworkCommand "curl") := by
  rfl

theorem disabled_network_readOnly_tailscale_ping_denies
    (rest : List String) :
    validateNetworkMode .readOnly .disabled
        { command := "tailscale", lookupCommand := "tailscale", args := "ping" :: rest } =
      .deny (.disabledNetworkCommand "tailscale") := by
  rfl

theorem disabled_network_readOnly_tailscale_netcheck_denies
    (rest : List String) :
    validateNetworkMode .readOnly .disabled
        { command := "tailscale", lookupCommand := "tailscale", args := "netcheck" :: rest } =
      .deny (.disabledNetworkCommand "tailscale") := by
  rfl

theorem workspaceWrite_requires_enforced_sandbox
    (support : RuntimeSupport)
    (sandbox : SandboxKind)
    (hselected :
      selectSandbox support .workspaceWrite = .selected sandbox) :
    support.workspaceWriteSandboxEnforced = true ∧
      sandbox.isWriteKernelSandbox = true := by
  cases support with
  | mk backend =>
      cases backend <;> simp [selectSandbox, RuntimeSupport.workspaceWriteSandboxEnforced] at hselected ⊢
      · subst hselected; rfl
      · subst hselected; rfl

theorem workspaceWrite_without_enforced_sandbox_denies :
    selectSandbox { sandboxBackend := .none } .workspaceWrite =
      .denied .workspaceWriteSandboxUnavailable := by
  rfl

theorem unrestricted_is_explicitly_unsandboxed
    (support : RuntimeSupport) :
    selectSandbox support .unrestricted =
      .selected .unsandboxedUnrestricted := by
  cases support with
  | mk backend => cases backend <;> rfl

/-- A write-capable bounded command never consults the read-only allowlist: what
a binary declares about its effects cannot widen a workspace or artifact
command, whose bound is the kernel sandbox. -/
theorem bounded_modes_ignore_declared_effects
    (policy : Policy) (request : CommandRequest) (allowlist : List String)
    (bounded : policy.mode = .workspaceWrite ∨ policy.mode = .artifactWrite) :
    validatePolicy { policy with readOnlyAllowlist := allowlist } request =
      validatePolicy policy request := by
  rcases policy with ⟨mode, allowed, forbidden, network, readOnly⟩
  simp only at bounded
  rcases bounded with rfl | rfl <;>
    cases network <;>
    simp [validatePolicy, validateAfterPrefixes, validateNetworkMode] <;>
    split <;> simp_all

theorem bounded_modes_select_only_the_kernel_sandbox
    (support : RuntimeSupport) (mode : ExecutionMode) (sandbox : SandboxKind)
    (bounded : mode = .workspaceWrite ∨ mode = .artifactWrite)
    (selected : selectSandbox support mode = .selected sandbox) :
    sandbox.isWriteKernelSandbox = true := by
  rcases support with ⟨backend⟩
  rcases bounded with rfl | rfl <;> cases backend <;> simp [selectSandbox] at selected <;> subst selected <;> rfl

/-- On a host with a sandbox backend, a read-only command is contained by the
read-only profile whatever the allowlist admits: it and every process it starts write nothing, inspect but cannot signal
processes outside its sandbox, and reach the network only as a network reader
under a policy that has not disabled it. The allowlist and the argument
validators decide only whether it starts. -/
theorem read_only_effects_are_bounded_by_the_read_only_profile
    (support : RuntimeSupport) (policy : Policy) (request : CommandRequest)
    (allowlist : List String)
    (readOnly : policy.mode = .readOnly)
    (backend : support.workspaceWriteSandboxEnforced = true) :
    commandProfile support { policy with readOnlyAllowlist := allowlist } request =
      some { writes := .nothing
             network := readOnlyReachesNetwork request && policy.networkMode.allowsNetwork
             processInfo := .all } := by
  rcases support with ⟨sb⟩
  rcases policy with ⟨mode, allowed, forbidden, network, listed⟩
  simp only at readOnly
  subst readOnly
  cases sb with
  | none => simp [RuntimeSupport.workspaceWriteSandboxEnforced] at backend
  | macosSeatbelt =>
      simp only [commandProfile, selectSandbox, seatbeltProfile]
      cases network <;> rfl
  | linuxLandlock =>
      simp only [commandProfile, selectSandbox, seatbeltProfile]
      cases network <;> rfl

/-- Without a sandbox backend no kernel profile contains a read-only command:
the allowlist, the argument validators and the launch options
(`readOnlyLaunchArgs`) are its whole bound. -/
theorem read_only_without_a_backend_has_no_kernel_profile
    (support : RuntimeSupport) (policy : Policy) (request : CommandRequest)
    (readOnly : policy.mode = .readOnly)
    (noBackend : support.workspaceWriteSandboxEnforced = false) :
    commandProfile support policy request = none := by
  rcases support with ⟨sb⟩
  cases sb with
  | none =>
      simp [commandProfile, selectSandbox, readOnly, seatbeltProfile]
  | macosSeatbelt => simp [RuntimeSupport.workspaceWriteSandboxEnforced] at noBackend
  | linuxLandlock => simp [RuntimeSupport.workspaceWriteSandboxEnforced] at noBackend

/-- `sed`'s validator admits a script's `w` command: the read-only profile
refuses the write, and where no profile contains the command, GNU `--sandbox`
refuses the script. -/
theorem read_only_sed_write_command_is_fenced_at_launch :
    let request : CommandRequest :=
      { command := "sed", lookupCommand := "sed", args := ["-n", "w copy.txt", "input.txt"] }
    validateReadOnlyArgs request = .allow ∧
      readOnlyLaunchArgs false request = ["--sandbox", "-n", "w copy.txt", "input.txt"] := by
  native_decide

theorem filtered_env_excludes_KEY
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .key = none := by
  rfl

theorem filtered_env_excludes_SECRET
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .secret = none := by
  rfl

theorem filtered_env_excludes_TOKEN
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .token = none := by
  rfl

theorem filtered_env_forces_PAGER
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .pager = some .forcedCat := by
  rfl

theorem filtered_env_forces_GIT_PAGER
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .gitPager = some .forcedCat := by
  rfl

theorem filtered_env_forces_NO_COLOR
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .noColor = some .forcedNoColor := by
  rfl

theorem filtered_env_forces_CLICOLOR
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .cliColor = some .forcedCliColorOff := by
  rfl

theorem filtered_env_forces_TERM
    (inputHas : EnvKey → Bool) :
    filteredEnv inputHas .term = some .forcedDumb := by
  rfl

end CommandPolicy

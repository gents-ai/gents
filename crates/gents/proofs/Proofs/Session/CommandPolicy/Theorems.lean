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
    (program : String)
    (hselected :
      selectSandbox support .workspaceWrite program = .selected sandbox) :
    support.workspaceWriteSandboxEnforced = true ∧
      sandbox = .macosSeatbelt := by
  cases support with
  | mk enforced =>
      cases enforced
      · simp [selectSandbox] at hselected
      · simp [selectSandbox] at hselected ⊢
        exact hselected.symm

theorem workspaceWrite_without_enforced_sandbox_denies (program : String) :
    selectSandbox { workspaceWriteSandboxEnforced := false } .workspaceWrite program =
      .denied .workspaceWriteSandboxUnavailable := by
  rfl

theorem unrestricted_is_explicitly_unsandboxed
    (support : RuntimeSupport) (program : String) :
    selectSandbox support .unrestricted program =
      .selected .unsandboxedUnrestricted := by
  rfl

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
    (program : String)
    (bounded : mode = .workspaceWrite ∨ mode = .artifactWrite)
    (selected : selectSandbox support mode program = .selected sandbox) :
    sandbox = .macosSeatbelt := by
  rcases support with ⟨enforced⟩
  rcases bounded with rfl | rfl <;> cases enforced <;> simp_all [selectSandbox]

/-- On a host with a sandbox backend, a read-only command whose program
Seatbelt can run is contained by the read-only profile whatever the allowlist
admits: it and every process it starts write nothing, inspect but cannot signal
processes outside its sandbox, and reach the network only as a network reader
under a policy that has not disabled it. The allowlist and the argument
validators decide only whether it starts. -/
theorem read_only_effects_are_bounded_by_the_read_only_profile
    (support : RuntimeSupport) (policy : Policy) (request : CommandRequest)
    (program : String) (allowlist : List String)
    (readOnly : policy.mode = .readOnly)
    (backend : support.workspaceWriteSandboxEnforced = true)
    (runnable : seatbeltRefusedPrograms.contains program = false) :
    commandProfile support { policy with readOnlyAllowlist := allowlist } request program =
      some { writes := .nothing
             network := readOnlyReachesNetwork request && policy.networkMode.allowsNetwork
             processInfo := .all } := by
  rcases policy with ⟨mode, allowed, forbidden, network, listed⟩
  simp only at readOnly
  subst readOnly
  simp only [commandProfile, selectSandbox, backend, runnable]
  cases network <;> rfl

/-- Without a sandbox backend no kernel profile contains a read-only command:
the allowlist, the argument validators and the launch options
(`readOnlyLaunchArgs`) are its whole bound. -/
theorem read_only_without_a_backend_has_no_kernel_profile
    (support : RuntimeSupport) (policy : Policy) (request : CommandRequest)
    (program : String)
    (readOnly : policy.mode = .readOnly)
    (noBackend : support.workspaceWriteSandboxEnforced = false) :
    commandProfile support policy request program = none := by
  rcases policy with ⟨mode, allowed, forbidden, network, listed⟩
  simp only at readOnly
  subst readOnly
  simp [commandProfile, selectSandbox, noBackend, seatbeltProfile]

theorem seatbelt_refused_programs_run_under_their_validators
    (support : RuntimeSupport) (program : String)
    (refused : seatbeltRefusedPrograms.contains program = true) :
    selectSandbox support .readOnly program = .selected .policyReadOnly := by
  rcases support with ⟨enforced⟩
  simp only [selectSandbox, refused]
  cases enforced <;> rfl

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

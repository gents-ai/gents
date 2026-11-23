import Proofs.Session.CommandPolicy.Validation
import Proofs.Session.CommandPolicy.Sandbox
import Proofs.Session.CommandPolicy.Env

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
      sandbox = .macosSeatbelt := by
  cases support with
  | mk enforced =>
      cases enforced
      · simp [selectSandbox] at hselected
      · simp [selectSandbox] at hselected ⊢
        exact hselected.symm

theorem workspaceWrite_without_enforced_sandbox_denies :
    selectSandbox { workspaceWriteSandboxEnforced := false } .workspaceWrite =
      .denied .workspaceWriteSandboxUnavailable := by
  rfl

theorem unrestricted_is_explicitly_unsandboxed
    (support : RuntimeSupport) :
    selectSandbox support .unrestricted =
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
    (bounded : mode = .workspaceWrite ∨ mode = .artifactWrite)
    (selected : selectSandbox support mode = .selected sandbox) :
    sandbox = .macosSeatbelt := by
  rcases support with ⟨enforced⟩
  rcases bounded with rfl | rfl <;> cases enforced <;> simp_all [selectSandbox]

/-- Read-only commands run under no kernel sandbox on any host: the allowlist
and the argument validators are their only bound, so a listed binary that has
an effect beyond reading under accepted arguments is not contained. -/
theorem read_only_runs_without_a_kernel_sandbox (support : RuntimeSupport) :
    selectSandbox support .readOnly = .selected .policyReadOnly := rfl

/-- `sed`'s validator refuses only in-place editing: a script's `w` command
writes the named file and is accepted. -/
theorem read_only_sed_accepts_a_write_command :
    validateReadOnlyArgs { command := "sed", lookupCommand := "sed",
      args := ["-n", "w copy.txt", "input.txt"] } = .allow := by decide

/-- `curl`'s validator accepts a cookie jar, which curl writes to the named file. -/
theorem read_only_curl_accepts_a_cookie_jar :
    validateReadOnlyArgs { command := "curl", lookupCommand := "curl",
      args := ["-c", "jar.txt", "https://example.com"] } = .allow := by decide

/-- `git grep -O<pager>` opens matches with the named program, which git runs. -/
theorem read_only_git_grep_accepts_a_pager_program :
    validateReadOnlyArgs { command := "git", lookupCommand := "git",
      args := ["grep", "-Otouch", "pattern"] } = .allow := by decide

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

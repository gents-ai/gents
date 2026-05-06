import Proofs.CommandPolicy.Validation
import Proofs.CommandPolicy.Sandbox
import Proofs.CommandPolicy.Env

/-!
# Command Policy Theorems

Safety properties for command/tool execution policy.
-/

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
      commandAllowlisted request.command allowlist = false) :
    validateReadOnlyCommand allowlist request =
      .deny (.readOnlyCommandNotAllowlisted request.command) := by
  simp [validateReadOnlyCommand, hunallowlisted]

theorem readOnly_validator_allows_only_allowlisted
    (allowlist : List String)
    (request : CommandRequest)
    (hallow : validateReadOnlyCommand allowlist request = .allow) :
    commandAllowlisted request.command allowlist = true := by
  cases hlisted : commandAllowlisted request.command allowlist with
  | false =>
      simp [validateReadOnlyCommand, hlisted] at hallow
  | true =>
      rfl

theorem disabled_network_unrestricted_fails_closed
    (request : CommandRequest) :
    validateNetworkMode .unrestricted .disabled request =
      .deny .disabledNetworkUnenforceable := by
  rfl

theorem disabled_network_readOnly_curl_denies
    (args : List String) :
    validateNetworkMode .readOnly .disabled
        { command := "curl", args := args } =
      .deny (.disabledNetworkCommand "curl") := by
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

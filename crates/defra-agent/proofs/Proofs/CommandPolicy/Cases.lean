import Proofs.CommandPolicy.Validation
import Proofs.CommandPolicy.Sandbox
import Proofs.CommandPolicy.Env

/-!
# Command Policy Generated Cases

Finite executable witnesses for the Rust command-policy conformance tests.
These cases intentionally cover only local policy ordering, sandbox selection
labels, and shell-environment filtering. External binary read-only behavior and
host kernel sandbox correctness remain conformance boundaries.
-/

namespace CommandPolicy

structure CommandPolicyCase where
  name : String
  category : String
  policy : Policy
  request : CommandRequest
  decision : Decision
  deriving Repr

structure CommandSandboxCase where
  name : String
  category : String
  mode : ExecutionMode
  workspaceWriteSandboxEnforced : Bool
  decision : SandboxDecision
  deriving Repr

structure CommandEnvCase where
  name : String
  envKey : EnvKey
  inputPresent : Bool
  inputName : String
  inputValue : String
  outputName : String
  expected : Option EnvValue
  deriving Repr

def commandPolicy
    (mode : ExecutionMode)
    (allowed forbidden : List (List String))
    (network : NetworkMode)
    (allowlist : List String) : Policy :=
  { mode := mode
  , allowedArgvPrefixes := allowed
  , forbiddenArgvPrefixes := forbidden
  , networkMode := network
  , readOnlyAllowlist := allowlist
  }

def commandRequest
    (command lookupCommand : String)
    (args : List String) : CommandRequest :=
  { command := command, lookupCommand := lookupCommand, args := args }

def validationCase
    (name category : String)
    (policy : Policy)
    (request : CommandRequest) : CommandPolicyCase :=
  { name := name
  , category := category
  , policy := policy
  , request := request
  , decision := validatePolicy policy request
  }

def sandboxCase
    (name category : String)
    (mode : ExecutionMode)
    (workspaceWriteSandboxEnforced : Bool) : CommandSandboxCase :=
  let support := { workspaceWriteSandboxEnforced := workspaceWriteSandboxEnforced }
  { name := name
  , category := category
  , mode := mode
  , workspaceWriteSandboxEnforced := workspaceWriteSandboxEnforced
  , decision := selectSandbox support mode
  }

def envInput
    (target : EnvKey)
    (present : Bool) : EnvKey → Bool :=
  fun candidate => if candidate = target then present else false

def envCase
    (name : String)
    (envKey : EnvKey)
    (inputPresent : Bool)
    (inputValue : String := "input-value") : CommandEnvCase :=
  { name := name
  , envKey := envKey
  , inputPresent := inputPresent
  , inputName := envKey.sampleName
  , inputValue := inputValue
  , outputName := envKey.sampleName
  , expected := filteredEnv (envInput envKey inputPresent) envKey
  }

def commandPolicyCases : List CommandPolicyCase :=
  [ validationCase
      "forbidden_prefix_wins_over_allowed_prefix_order"
      "forbidden_prefix"
      (commandPolicy
        .readOnly
        [["git", "status"]]
        [["git"], ["git", "status", "--short"]]
        .inherit
        ["git"])
      (commandRequest "git" "git" ["status", "--short"])
  , validationCase
      "forbidden_prefix_second_configured_match"
      "forbidden_prefix"
      (commandPolicy
        .readOnly
        []
        [["git", "status", "--short"], ["git", "diff"]]
        .inherit
        ["git"])
      (commandRequest "git" "git" ["diff", "--stat"])
  , validationCase
      "allowed_prefix_required_precedes_network_and_allowlist"
      "allowed_prefix_required"
      (commandPolicy
        .readOnly
        [["git", "status"]]
        []
        .disabled
        [])
      (commandRequest "curl" "curl" ["https://example.com"])
  , validationCase
      "read_only_allowlisted_lookup_basename_allows"
      "read_only_allowlist"
      (commandPolicy .readOnly [] [] .inherit ["cat"])
      (commandRequest "/bin/cat" "cat" ["README.md"])
  , validationCase
      "read_only_unallowlisted_denies"
      "read_only_allowlist"
      (commandPolicy .readOnly [] [] .inherit ["git"])
      (commandRequest "cat" "cat" ["README.md"])
  , validationCase
      "disabled_network_unrestricted_fails_closed"
      "disabled_network_failure"
      (commandPolicy .unrestricted [] [] .disabled [])
      (commandRequest "printf" "printf" ["ok"])
  , validationCase
      "disabled_network_read_only_curl_denies_before_allowlist"
      "disabled_network_failure"
      (commandPolicy .readOnly [] [] .disabled [])
      (commandRequest "/usr/bin/curl" "curl" ["https://example.com"])
  , validationCase
      "disabled_network_read_only_tailscale_ping_denies"
      "disabled_network_failure"
      (commandPolicy .readOnly [] [] .disabled ["tailscale"])
      (commandRequest "tailscale" "tailscale" ["ping", "100.64.0.1"])
  , validationCase
      "disabled_network_read_only_tailscale_netcheck_denies"
      "disabled_network_failure"
      (commandPolicy .readOnly [] [] .disabled ["tailscale"])
      (commandRequest "tailscale" "tailscale" ["netcheck"])
  , validationCase
      "disabled_network_workspace_write_validates_for_sandbox_enforcement"
      "disabled_network_failure"
      (commandPolicy .workspaceWrite [] [] .disabled [])
      (commandRequest "printf" "printf" ["ok"])
  ]

def commandSandboxCases : List CommandSandboxCase :=
  [ sandboxCase
      "read_only_selects_policy_read_only"
      "read_only_sandbox_selection"
      .readOnly
      false
  , sandboxCase
      "workspace_write_enforced_selects_macos_seatbelt"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      true
  , sandboxCase
      "workspace_write_unenforced_denies"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      false
  , sandboxCase
      "unrestricted_selects_unsandboxed_unrestricted"
      "unrestricted_unsandboxed_labeling"
      .unrestricted
      false
  ]

def commandEnvCases : List CommandEnvCase :=
  [ envCase "env_path_inherited" .path true "/custom/bin"
  , envCase "env_path_fallback_when_absent" .path false
  , envCase "env_home_inherited" .home true "/tmp/home"
  , envCase "env_home_absent_dropped" .home false
  , envCase "env_key_marker_dropped" .key true "secret"
  , envCase "env_secret_marker_dropped" .secret true "secret"
  , envCase "env_token_marker_dropped" .token true "secret"
  , envCase "env_other_key_dropped" .other true "drop"
  , envCase "env_pager_forced_cat" .pager true "less"
  , envCase "env_pager_absent_still_forced_cat" .pager false
  , envCase "env_git_pager_forced_cat" .gitPager true "less"
  , envCase "env_no_color_forced_on" .noColor false
  , envCase "env_clicolor_forced_off" .cliColor false
  , envCase "env_term_forced_dumb" .term true "xterm-256color"
  ]

end CommandPolicy

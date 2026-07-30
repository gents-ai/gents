import Proofs.CommandPolicy.Validation
import Proofs.CommandPolicy.Sandbox
import Proofs.CommandPolicy.Env

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

def defaultReadOnlyAllowlist : List String :=
  [ "pwd"
  , "ls"
  , "find"
  , "cat"
  , "head"
  , "tail"
  , "sed"
  , "grep"
  , "rg"
  , "wc"
  , "stat"
  , "file"
  , "git"
  , "date"
  , "hostname"
  , "uptime"
  , "df"
  , "vm_stat"
  , "ps"
  , "lsof"
  , "curl"
  , "launchctl"
  , "tailscale"
  , "sudo"
  ]

def readOnlySafetyPolicy : Policy :=
  commandPolicy .readOnly [] [] .inherit defaultReadOnlyAllowlist

def readOnlySafetyCase
    (name command lookupCommand : String)
    (args : List String) : CommandPolicyCase :=
  validationCase
    name
    "read_only_argv_safety"
    readOnlySafetyPolicy
    (commandRequest command lookupCommand args)

def commandPolicyOrderingCases : List CommandPolicyCase :=
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
      "allowed_prefix_authorizes_read_only_diagnostic_command"
      "read_only_configured_prefix"
      (commandPolicy
        .readOnly
        [["spctl", "--assess", "--type", "execute"]]
        []
        .inherit
        defaultReadOnlyAllowlist)
      (commandRequest
        "spctl"
        "spctl"
        ["--assess", "--type", "execute", "/Applications/Gents.app"])
  , validationCase
      "forbidden_prefix_overrides_configured_read_only_diagnostic"
      "read_only_configured_prefix"
      (commandPolicy
        .readOnly
        [["spctl", "--assess"]]
        [["spctl", "--assess", "--raw"]]
        .inherit
        defaultReadOnlyAllowlist)
      (commandRequest
        "spctl"
        "spctl"
        ["--assess", "--raw", "/Applications/Gents.app"])
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

def readOnlySafetyCases : List CommandPolicyCase :=
  [ readOnlySafetyCase
      "read_only_git_status_allows"
      "git"
      "git"
      ["status", "--short"]
  , readOnlySafetyCase
      "read_only_git_branch_list_allows"
      "git"
      "git"
      ["branch", "--list", "--format=%(refname:short)"]
  , readOnlySafetyCase
      "read_only_sed_print_allows"
      "sed"
      "sed"
      ["-n", "1,10p", "README.md"]
  , readOnlySafetyCase
      "read_only_find_type_file_allows"
      "find"
      "find"
      [".", "-maxdepth", "2", "-type", "f"]
  , readOnlySafetyCase
      "read_only_rg_search_allows"
      "rg"
      "rg"
      ["-n", "needle", "."]
  , readOnlySafetyCase
      "read_only_curl_http_get_allows"
      "curl"
      "curl"
      ["-fsS", "https://example.com/metrics"]
  , readOnlySafetyCase
      "read_only_launchctl_print_allows"
      "launchctl"
      "launchctl"
      ["print", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_tailscale_status_allows"
      "tailscale"
      "tailscale"
      ["status"]
  , readOnlySafetyCase
      "read_only_tailscale_netcheck_allows"
      "tailscale"
      "tailscale"
      ["netcheck"]
  , readOnlySafetyCase
      "read_only_sudo_launchctl_print_allows"
      "sudo"
      "sudo"
      ["/bin/launchctl", "print", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_git_global_config_denies"
      "git"
      "git"
      ["-c", "core.pager=cat", "status"]
  , readOnlySafetyCase
      "read_only_git_config_env_denies"
      "git"
      "git"
      ["--config-env=GIT_CONFIG_GLOBAL=ENV_FILE", "status"]
  , readOnlySafetyCase
      "read_only_git_output_flag_denies"
      "git"
      "git"
      ["diff", "--output=/tmp/gents-diff.txt"]
  , readOnlySafetyCase
      "read_only_git_exec_flag_denies"
      "git"
      "git"
      ["log", "--exec=touch /tmp/gents-nope"]
  , readOnlySafetyCase
      "read_only_git_commit_subcommand_denies"
      "git"
      "git"
      ["commit", "-m", "nope"]
  , readOnlySafetyCase
      "read_only_git_branch_delete_denies"
      "git"
      "git"
      ["branch", "-D", "main"]
  , readOnlySafetyCase
      "read_only_sed_in_place_short_denies"
      "sed"
      "sed"
      ["-i", "s/a/b/g", "README.md"]
  , readOnlySafetyCase
      "read_only_sed_in_place_long_denies"
      "sed"
      "sed"
      ["--in-place", "s/a/b/g", "README.md"]
  , readOnlySafetyCase
      "read_only_sed_in_place_suffix_denies"
      "sed"
      "sed"
      ["--in-place=.bak", "s/a/b/g", "README.md"]
  , readOnlySafetyCase
      "read_only_find_delete_denies"
      "find"
      "find"
      [".", "-delete"]
  , readOnlySafetyCase
      "read_only_find_exec_denies"
      "find"
      "find"
      [".", "-exec", "rm", "{}", ";"]
  , readOnlySafetyCase
      "read_only_find_fprint_denies"
      "find"
      "find"
      [".", "-fprint0", "out"]
  , readOnlySafetyCase
      "read_only_rg_pre_denies"
      "rg"
      "rg"
      ["--pre", "touch /tmp/gents-nope", "needle"]
  , readOnlySafetyCase
      "read_only_rg_search_zip_denies"
      "rg"
      "rg"
      ["--search-zip", "needle"]
  , readOnlySafetyCase
      "read_only_curl_post_denies"
      "curl"
      "curl"
      ["-X", "POST", "https://example.com/graphql"]
  , readOnlySafetyCase
      "read_only_curl_data_denies"
      "curl"
      "curl"
      ["--data={}", "https://example.com/graphql"]
  , readOnlySafetyCase
      "read_only_curl_output_denies"
      "curl"
      "curl"
      ["-o", "/tmp/gents-out", "https://example.com/metrics"]
  , readOnlySafetyCase
      "read_only_curl_upload_denies"
      "curl"
      "curl"
      ["-T", "payload.json", "https://example.com/upload"]
  , readOnlySafetyCase
      "read_only_curl_config_denies"
      "curl"
      "curl"
      ["-K", "curlrc", "https://example.com/metrics"]
  , readOnlySafetyCase
      "read_only_curl_missing_http_url_denies"
      "curl"
      "curl"
      ["-fsS", "example.com/metrics"]
  , readOnlySafetyCase
      "read_only_launchctl_bootout_denies"
      "launchctl"
      "launchctl"
      ["bootout", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_launchctl_missing_subcommand_denies"
      "launchctl"
      "launchctl"
      []
  , readOnlySafetyCase
      "read_only_tailscale_up_denies"
      "tailscale"
      "tailscale"
      ["up"]
  , readOnlySafetyCase
      "read_only_sudo_launchctl_wrong_path_denies"
      "sudo"
      "sudo"
      ["/usr/bin/launchctl", "print", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_sudo_rm_denies"
      "sudo"
      "sudo"
      ["rm", "-rf", "/tmp/gents-nope"]
  , readOnlySafetyCase
      "read_only_sudo_missing_command_denies"
      "sudo"
      "sudo"
      []
  ]

def commandPolicyCases : List CommandPolicyCase :=
  commandPolicyOrderingCases ++ readOnlySafetyCases

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

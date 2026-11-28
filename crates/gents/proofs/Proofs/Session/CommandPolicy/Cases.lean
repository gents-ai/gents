import Proofs.Session.CommandPolicy.Validation
import Proofs.Session.CommandPolicy.Sandbox
import Proofs.Session.CommandPolicy.Env

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
  sandboxBackend : SandboxBackend
  lookupCommand : String
  networkMode : NetworkMode
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
    (sandboxBackend : SandboxBackend)
    (lookupCommand : String := "git")
    (network : NetworkMode := .inherit) : CommandSandboxCase :=
  let support := { sandboxBackend := sandboxBackend }
  { name := name
  , category := category
  , mode := mode
  , sandboxBackend := sandboxBackend
  , lookupCommand := lookupCommand
  , networkMode := network
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

/-- The commands the read-only tool runs without a configured allowed prefix.
Each runs under the read-only Seatbelt profile. `ps` and `sudo` are setuid root,
which the kernel refuses to execute inside a sandbox, and launchd refuses
`launchctl list` to a sandboxed caller, so none of the three is listed: each is
refused as any unlisted command is. -/
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
  , "cmp"
  , "sha256sum"
  , "file"
  , "git"
  , "date"
  , "hostname"
  , "uptime"
  , "df"
  , "vm_stat"
  , "lsof"
  , "curl"
  , "tailscale"
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

/-- Arguments that write a file, run a program or change the host, refused on
every host; on a host without a sandbox backend they are the whole fence. -/
def readOnlyWriteAndExecCases : List CommandPolicyCase :=
  [ readOnlySafetyCase "read_only_sed_in_place_in_a_cluster_denies" "sed" "sed"
      ["-ni", "p", "README.md"]
  , readOnlySafetyCase "read_only_sed_bsd_in_place_denies" "sed" "sed"
      ["-I", ".bak", "s/a/b/", "README.md"]
  , readOnlySafetyCase "read_only_sed_in_place_abbreviation_denies" "sed" "sed"
      ["--in", "s/a/b/", "README.md"]
  , readOnlySafetyCase "read_only_sed_expression_value_is_not_an_option" "sed" "sed"
      ["-es/i/x/", "README.md"]
  , readOnlySafetyCase "read_only_sed_write_command_is_fenced_at_launch" "sed" "sed"
      ["-n", "w copy.txt", "README.md"]
  , readOnlySafetyCase "read_only_curl_cookie_jar_denies" "curl" "curl"
      ["-c", "jar.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_cookie_jar_abbreviation_denies" "curl" "curl"
      ["--cookie-j", "jar.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_cookie_jar_in_a_cluster_denies" "curl" "curl"
      ["-sc", "jar.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_dump_header_denies" "curl" "curl"
      ["-D", "headers.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_output_in_a_cluster_denies" "curl" "curl"
      ["-so", "out.html", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_remote_name_denies" "curl" "curl"
      ["-O", "https://example.com/file"]
  , readOnlySafetyCase "read_only_curl_trace_denies" "curl" "curl"
      ["--trace", "trace.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_trace_ascii_denies" "curl" "curl"
      ["--trace-ascii", "trace.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_libcurl_denies" "curl" "curl"
      ["--libcurl", "source.c", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_output_dir_denies" "curl" "curl"
      ["--output-dir", "downloads", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_stderr_file_denies" "curl" "curl"
      ["--stderr", "log.txt", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_write_out_file_denies" "curl" "curl"
      ["-w", "%output{status.txt}%{http_code}", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_protocol_override_denies" "curl" "curl"
      ["--proto", "=all", "https://example.com"]
  , readOnlySafetyCase "read_only_curl_trace_time_allows" "curl" "curl"
      ["-fsS", "--trace-time", "-H", "Accept: text/plain", "https://example.com"]
  , readOnlySafetyCase "read_only_git_grep_pager_program_denies" "git" "git"
      ["grep", "-Otouch", "pattern"]
  , readOnlySafetyCase "read_only_git_grep_pager_in_a_cluster_denies" "git" "git"
      ["grep", "-nO", "pattern"]
  , readOnlySafetyCase "read_only_git_grep_pager_abbreviation_denies" "git" "git"
      ["grep", "--op=touch", "pattern"]
  , readOnlySafetyCase "read_only_git_grep_context_allows" "git" "git"
      ["grep", "-nA2", "pattern"]
  , readOnlySafetyCase "read_only_git_show_signature_denies" "git" "git"
      ["log", "--show-signature"]
  , readOnlySafetyCase "read_only_git_signature_placeholder_denies" "git" "git"
      ["log", "--format=%H %G?"]
  , readOnlySafetyCase "read_only_git_textconv_abbreviation_denies" "git" "git"
      ["grep", "--textc", "pattern"]
  , readOnlySafetyCase "read_only_git_text_allows" "git" "git"
      ["grep", "--text", "pattern"]
  , readOnlySafetyCase "read_only_git_status_verbose_denies" "git" "git"
      ["status", "-v"]
  , readOnlySafetyCase "read_only_git_log_format_allows" "git" "git"
      ["log", "--format=%H %s", "-n", "5"]
  , readOnlySafetyCase "read_only_rg_search_zip_in_a_cluster_denies" "rg" "rg"
      ["-iz", "needle"]
  , readOnlySafetyCase "read_only_file_compile_denies" "file" "file"
      ["-C", "-m", "magic"]
  , readOnlySafetyCase "read_only_file_type_allows" "file" "file"
      ["-b", "README.md"]
  , readOnlySafetyCase "read_only_lsof_device_cache_denies" "lsof" "lsof"
      ["-Db"]
  , readOnlySafetyCase "read_only_tailscale_status_web_denies" "tailscale" "tailscale"
      ["status", "--web"]
  , readOnlySafetyCase "read_only_date_set_denies" "date" "date"
      ["-s", "2030-01-01"]
  , readOnlySafetyCase "read_only_date_operand_denies" "date" "date"
      ["010100002030"]
  , readOnlySafetyCase "read_only_date_format_allows" "date" "date"
      ["-u", "-d", "yesterday", "+%s"]
  , readOnlySafetyCase "read_only_hostname_set_denies" "hostname" "hostname"
      ["other-host"]
  , readOnlySafetyCase "read_only_hostname_short_allows" "hostname" "hostname"
      ["-s"]
  ]

def readOnlySafetyCases : List CommandPolicyCase :=
  [ readOnlySafetyCase "compare_backup_contents" "cmp" "cmp"
      ["/host/backups/records.txt", "/host/data/records.txt"]
  , readOnlySafetyCase "checksum_backup_contents" "sha256sum" "sha256sum"
      ["/host/backups/records.txt", "/host/data/records.txt"]
  , readOnlySafetyCase "checksum_manifest_verification_is_read_only" "sha256sum" "sha256sum"
      ["--check", "/host/checksums.txt"]
  , readOnlySafetyCase "comparison_does_not_admit_shell_execution" "sh" "sh"
      ["-c", "cmp a b"]
  , readOnlySafetyCase
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
      "read_only_ps_is_not_listed_denies"
      "ps"
      "ps"
      ["-o", "pid=", "-p", "1"]
  , readOnlySafetyCase
      "read_only_sudo_is_not_listed_denies"
      "sudo"
      "sudo"
      ["/bin/launchctl", "print", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_launchctl_is_not_listed_denies"
      "/bin/launchctl"
      "launchctl"
      ["print", "system/com.example.agent"]
  , readOnlySafetyCase
      "read_only_tailscale_up_denies"
      "tailscale"
      "tailscale"
      ["up"]
  ] ++ readOnlyWriteAndExecCases

def commandPolicyCases : List CommandPolicyCase :=
  commandPolicyOrderingCases ++ readOnlySafetyCases

def commandSandboxCases : List CommandSandboxCase :=
  [ sandboxCase
      "read_only_with_backend_selects_the_read_only_profile"
      "read_only_sandbox_selection"
      .readOnly
      .macosSeatbelt
  , sandboxCase
      "read_only_network_reader_reaches_the_network"
      "read_only_sandbox_selection"
      .readOnly
      .macosSeatbelt
      "curl"
  , sandboxCase
      "read_only_network_reader_under_disabled_network_is_offline"
      "read_only_sandbox_selection"
      .readOnly
      .macosSeatbelt
      "curl"
      .disabled
  , sandboxCase
      "read_only_with_linux_landlock_selects_the_read_only_profile"
      "read_only_sandbox_selection"
      .readOnly
      .linuxLandlock
  , sandboxCase
      "read_only_linux_network_reader_reaches_the_network"
      "read_only_sandbox_selection"
      .readOnly
      .linuxLandlock
      "curl"
  , sandboxCase
      "read_only_linux_network_reader_under_disabled_network_is_offline"
      "read_only_sandbox_selection"
      .readOnly
      .linuxLandlock
      "curl"
      .disabled
  , sandboxCase
      "read_only_without_backend_selects_policy_read_only"
      "read_only_sandbox_selection"
      .readOnly
      .none
  , sandboxCase
      "workspace_write_enforced_selects_macos_seatbelt"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      .macosSeatbelt
  , sandboxCase
      "workspace_write_enabled_network_reaches_the_network"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      .macosSeatbelt
      "git"
      .enabled
  , sandboxCase
      "workspace_write_enforced_selects_linux_landlock"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      .linuxLandlock
  , sandboxCase
      "workspace_write_linux_enabled_network_reaches_the_network"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      .linuxLandlock
      "git"
      .enabled
  , sandboxCase
      "workspace_write_unenforced_denies"
      "workspace_write_sandbox_selection"
      .workspaceWrite
      .none
  , sandboxCase
      "artifact_write_enforced_selects_macos_seatbelt"
      "artifact_write_sandbox_selection"
      .artifactWrite
      .macosSeatbelt
  , sandboxCase
      "artifact_write_enforced_selects_linux_landlock"
      "artifact_write_sandbox_selection"
      .artifactWrite
      .linuxLandlock
  , sandboxCase
      "artifact_write_unenforced_denies"
      "artifact_write_sandbox_selection"
      .artifactWrite
      .none
  , sandboxCase
      "unrestricted_selects_unsandboxed_unrestricted"
      "unrestricted_unsandboxed_labeling"
      .unrestricted
      .none
  ]

def commandEnvCases : List CommandEnvCase :=
  [ envCase "env_path_inherited" .path true "/custom/bin"
  , envCase "env_path_fallback_when_absent" .path false
  , envCase "env_home_inherited" .home true "/tmp/home"
  , envCase "env_home_absent_dropped" .home false
  , envCase "env_xdg_config_home_inherited" .xdgConfigHome true "/tmp/user-config"
  , envCase "env_xdg_runtime_dir_inherited" .xdgRuntimeDir true "/run/user/1001"
  , envCase "env_dbus_session_bus_inherited" .dbusSessionBusAddress true "unix:path=/run/user/1001/bus"
  , envCase "env_dbus_absent_stays_absent" .dbusSessionBusAddress false
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

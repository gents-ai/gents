import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Session.CommandPolicy.Cases
import Proofs.Session.CommandPolicy.Launch
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts

open Conformance.ContractCases

deriving instance Lean.ToJson for CommandPolicy.NetworkMode, CommandPolicy.EnvKey,
  CommandPolicy.EnvValue

structure PolicyInput where
  name : String
  category : String
  mode : CommandPolicy.ExecutionMode
  allowedArgvPrefixes : List (List String)
  forbiddenArgvPrefixes : List (List String)
  networkMode : CommandPolicy.NetworkMode
  readOnlyAllowlist : List String
  command : String
  lookupCommand : String
  args : List String
  deriving Lean.ToJson

structure PolicyOutput where
  decision : String
  denialReason : Option String
  matchedPrefix : Option (List String)
  deniedArgv : Option (List String)
  deniedCommand : Option String
  deniedArgument : Option String
  deniedSubcommand : Option String
  deriving Lean.ToJson

@[conformance_family]
def commandPolicyValidation : Family PolicyInput PolicyOutput where
  inputs := CommandPolicy.commandPolicyCases.map fun c =>
    { name := c.name, category := c.category, mode := c.policy.mode
      allowedArgvPrefixes := c.policy.allowedArgvPrefixes
      forbiddenArgvPrefixes := c.policy.forbiddenArgvPrefixes
      networkMode := c.policy.networkMode
      readOnlyAllowlist := c.policy.readOnlyAllowlist
      command := c.request.command, lookupCommand := c.request.lookupCommand
      args := c.request.args }
  model := fun i =>
    let decision := CommandPolicy.validatePolicy
      { mode := i.mode, allowedArgvPrefixes := i.allowedArgvPrefixes
        forbiddenArgvPrefixes := i.forbiddenArgvPrefixes, networkMode := i.networkMode
        readOnlyAllowlist := i.readOnlyAllowlist }
      { command := i.command, lookupCommand := i.lookupCommand, args := i.args }
    let reason := decision.denialReason?
    { decision := decision.toContract
      denialReason := reason.map CommandPolicy.DenialReason.toContract
      matchedPrefix := reason.bind CommandPolicy.DenialReason.matchedPrefix?
      deniedArgv := reason.bind CommandPolicy.DenialReason.argv?
      deniedCommand := reason.bind CommandPolicy.DenialReason.command?
      deniedArgument := reason.bind CommandPolicy.DenialReason.argument?
      deniedSubcommand := reason.bind CommandPolicy.DenialReason.subcommand? }

private def policyCase? (name : String) : Option CommandPolicy.CommandPolicyCase :=
  CommandPolicy.commandPolicyCases.find? (·.name == name)

private def denial (c : CommandPolicy.CommandPolicyCase) : String × Option String :=
  (c.decision.toContract, c.decision.denialReason?.map CommandPolicy.DenialReason.toContract)

example : [ "read_only_git_status_allows", "read_only_git_branch_list_allows",
    "read_only_sed_print_allows", "read_only_find_type_file_allows",
    "read_only_rg_search_allows", "read_only_curl_http_get_allows",
    "read_only_launchctl_print_allows", "read_only_tailscale_status_allows",
    "read_only_tailscale_netcheck_allows", "read_only_sudo_launchctl_print_allows",
    "allowed_prefix_authorizes_read_only_diagnostic_command" ].all
      (fun n => (policyCase? n).map denial == some ("allow", none)) = true := by
  native_decide

example : [ ("read_only_git_global_config_denies", "-c"),
    ("read_only_git_config_env_denies", "--config-env=GIT_CONFIG_GLOBAL=ENV_FILE"),
    ("read_only_git_output_flag_denies", "--output=/tmp/gents-diff.txt"),
    ("read_only_git_exec_flag_denies", "--exec=touch /tmp/gents-nope"),
    ("read_only_git_branch_delete_denies", "-D"),
    ("read_only_sed_in_place_short_denies", "-i"),
    ("read_only_sed_in_place_long_denies", "--in-place"),
    ("read_only_sed_in_place_suffix_denies", "--in-place=.bak"),
    ("read_only_find_delete_denies", "-delete"),
    ("read_only_find_exec_denies", "-exec"),
    ("read_only_find_fprint_denies", "-fprint0"),
    ("read_only_rg_pre_denies", "--pre"),
    ("read_only_rg_search_zip_denies", "--search-zip"),
    ("read_only_curl_post_denies", "-X"),
    ("read_only_curl_data_denies", "--data={}"),
    ("read_only_curl_output_denies", "-o"),
    ("read_only_curl_upload_denies", "-T"),
    ("read_only_curl_config_denies", "-K"),
    ("read_only_sudo_launchctl_wrong_path_denies", "/usr/bin/launchctl") ].all
      (fun (n, a) => (policyCase? n).map (fun c =>
        (denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.argument?)) ==
          some (("deny", some "readOnlyArgumentNotAllowed"), some a)) = true := by
  native_decide

example : [ ("read_only_sed_in_place_in_a_cluster_denies", "-ni"),
    ("read_only_sed_bsd_in_place_denies", "-I"),
    ("read_only_sed_in_place_abbreviation_denies", "--in"),
    ("read_only_curl_cookie_jar_denies", "-c"),
    ("read_only_curl_cookie_jar_abbreviation_denies", "--cookie-j"),
    ("read_only_curl_cookie_jar_in_a_cluster_denies", "-sc"),
    ("read_only_curl_dump_header_denies", "-D"),
    ("read_only_curl_output_in_a_cluster_denies", "-so"),
    ("read_only_curl_remote_name_denies", "-O"),
    ("read_only_curl_trace_denies", "--trace"),
    ("read_only_curl_trace_ascii_denies", "--trace-ascii"),
    ("read_only_curl_libcurl_denies", "--libcurl"),
    ("read_only_curl_output_dir_denies", "--output-dir"),
    ("read_only_curl_stderr_file_denies", "--stderr"),
    ("read_only_curl_write_out_file_denies", "%output{status.txt}%{http_code}"),
    ("read_only_curl_protocol_override_denies", "--proto"),
    ("read_only_git_grep_pager_program_denies", "-Otouch"),
    ("read_only_git_grep_pager_in_a_cluster_denies", "-nO"),
    ("read_only_git_grep_pager_abbreviation_denies", "--op=touch"),
    ("read_only_git_show_signature_denies", "--show-signature"),
    ("read_only_git_signature_placeholder_denies", "--format=%H %G?"),
    ("read_only_git_textconv_abbreviation_denies", "--textc"),
    ("read_only_git_status_verbose_denies", "-v"),
    ("read_only_rg_search_zip_in_a_cluster_denies", "-iz"),
    ("read_only_file_compile_denies", "-C"),
    ("read_only_lsof_device_cache_denies", "-Db"),
    ("read_only_tailscale_status_web_denies", "--web"),
    ("read_only_date_set_denies", "-s"),
    ("read_only_date_operand_denies", "010100002030"),
    ("read_only_hostname_set_denies", "other-host") ].all
      (fun (n, a) => (policyCase? n).map (fun c =>
        (denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.argument?)) ==
          some (("deny", some "readOnlyArgumentNotAllowed"), some a)) = true := by
  native_decide

example : [ "read_only_sed_expression_value_is_not_an_option",
    "read_only_sed_write_command_is_fenced_at_launch", "read_only_curl_trace_time_allows",
    "read_only_git_grep_context_allows", "read_only_git_text_allows",
    "read_only_git_log_format_allows", "read_only_file_type_allows",
    "read_only_date_format_allows", "read_only_hostname_short_allows" ].all
      (fun n => (policyCase? n).map denial == some ("allow", none)) = true := by
  native_decide

example : [ ("read_only_git_commit_subcommand_denies", "commit"),
    ("read_only_launchctl_bootout_denies", "bootout"),
    ("read_only_tailscale_up_denies", "up"),
    ("read_only_sudo_rm_denies", "rm") ].all
      (fun (n, s) => (policyCase? n).map (fun c =>
        (denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.subcommand?)) ==
          some (("deny", some "readOnlySubcommandNotAllowlisted"), some s)) = true := by
  native_decide

example : [ "read_only_launchctl_missing_subcommand_denies",
    "read_only_sudo_missing_command_denies" ].all
      (fun n => (policyCase? n).map denial ==
        some ("deny", some "readOnlySubcommandRequired")) = true := by
  native_decide

example : (policyCase? "read_only_curl_missing_http_url_denies").map (fun c =>
    (denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.command?)) =
      some (("deny", some "readOnlyUrlRequired"), some "curl") := by native_decide

example : (policyCase? "forbidden_prefix_wins_over_allowed_prefix_order").map (fun c =>
    (c.category, denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.matchedPrefix?)) =
      some ("forbidden_prefix", ("deny", some "forbiddenPrefix"), some ["git"]) := by native_decide

example : (policyCase? "forbidden_prefix_second_configured_match").map (fun c =>
    c.decision.denialReason?.bind CommandPolicy.DenialReason.matchedPrefix?) =
      some (some ["git", "diff"]) := by native_decide

example : (policyCase? "allowed_prefix_required_precedes_network_and_allowlist").map (fun c =>
    (denial c, c.decision.denialReason?.bind CommandPolicy.DenialReason.argv?)) =
      some (("deny", some "allowedPrefixRequired"), some ["curl", "https://example.com"]) := by
  native_decide

example : (policyCase? "allowed_prefix_authorizes_read_only_diagnostic_command").map
    (·.category) = some "read_only_configured_prefix" := by native_decide

example : (policyCase? "forbidden_prefix_overrides_configured_read_only_diagnostic").map denial =
    some ("deny", some "forbiddenPrefix") := by native_decide

example : (policyCase? "disabled_network_read_only_curl_denies_before_allowlist").map (fun c =>
    (c.decision.denialReason?.map CommandPolicy.DenialReason.toContract,
      c.decision.denialReason?.bind CommandPolicy.DenialReason.command?)) =
      some (some "disabledNetworkCommand", some "curl") := by native_decide

structure SandboxInput where
  name : String
  category : String
  mode : CommandPolicy.ExecutionMode
  workspaceWriteSandboxEnforced : Bool
  program : String
  lookupCommand : String
  networkMode : CommandPolicy.NetworkMode
  deriving Lean.ToJson

structure SandboxOutput where
  decision : String
  sandbox : Option String
  denialReason : Option String
  writes : Option String
  network : Option Bool
  processInfo : Option String
  deriving Lean.ToJson

/-- The sandbox a command runs under on a host that does or does not have a
sandbox backend, and the kernel profile that sandbox applies: production
`select_sandbox` and `seatbelt_profile`, given the host fact and the resolved
program their caller reads. -/
@[conformance_family]
def commandSandboxSelection : Family SandboxInput SandboxOutput where
  inputs := CommandPolicy.commandSandboxCases.map fun c =>
    { name := c.name, category := c.category, mode := c.mode
      workspaceWriteSandboxEnforced := c.workspaceWriteSandboxEnforced
      program := c.program, lookupCommand := c.lookupCommand, networkMode := c.networkMode }
  model := fun i =>
    let support : CommandPolicy.RuntimeSupport :=
      { workspaceWriteSandboxEnforced := i.workspaceWriteSandboxEnforced }
    let decision := CommandPolicy.selectSandbox support i.mode i.program
    let profile := CommandPolicy.commandProfile support
      { mode := i.mode, allowedArgvPrefixes := [], forbiddenArgvPrefixes := []
        networkMode := i.networkMode, readOnlyAllowlist := [] }
      { command := i.program, lookupCommand := i.lookupCommand, args := [] }
      i.program
    { decision := decision.toContract
      sandbox := decision.sandbox?.map CommandPolicy.SandboxKind.toContract
      denialReason := decision.denialReason?.map CommandPolicy.DenialReason.toContract
      writes := profile.map (·.writes.toContract)
      network := profile.map (·.network)
      processInfo := profile.map (·.processInfo.toContract) }

private def sandboxCase? (name : String) : Option CommandPolicy.CommandSandboxCase :=
  CommandPolicy.commandSandboxCases.find? (·.name == name)

example : (sandboxCase? "workspace_write_enforced_selects_macos_seatbelt").map (·.decision) =
    some (.selected .macosSeatbelt) := by native_decide

example : (sandboxCase? "unrestricted_selects_unsandboxed_unrestricted").map (·.decision) =
    some (.selected .unsandboxedUnrestricted) := by native_decide

example : [ "read_only_with_backend_selects_the_read_only_profile",
    "read_only_network_reader_reaches_the_network",
    "read_only_network_reader_under_disabled_network_is_offline" ].all
      (fun n => (sandboxCase? n).map (·.decision) ==
        some (.selected .macosSeatbeltReadOnly)) = true := by native_decide

example : [ "read_only_setuid_ps_runs_under_its_validator",
    "read_only_setuid_sudo_runs_under_its_validator",
    "read_only_launchctl_runs_under_its_validator",
    "read_only_without_backend_selects_policy_read_only" ].all
      (fun n => (sandboxCase? n).map (·.decision) ==
        some (.selected .policyReadOnly)) = true := by native_decide

structure LaunchInput where
  name : String
  kernelContained : Bool
  lookupCommand : String
  args : List String
  deriving Lean.ToJson

structure LaunchOutput where
  args : List String
  deriving Lean.ToJson

def readOnlyLaunchCases : List LaunchInput :=
  [ { name := "git_status_runs_without_configured_programs", kernelContained := true
      lookupCommand := "git", args := ["status", "--short"] }
  , { name := "git_diff_runs_without_external_diff_or_textconv", kernelContained := false
      lookupCommand := "git", args := ["--no-pager", "diff", "--stat", "HEAD"] }
  , { name := "git_log_runs_without_external_diff_or_textconv", kernelContained := true
      lookupCommand := "git", args := ["log", "-p", "-1"] }
  , { name := "git_grep_runs_without_configured_programs", kernelContained := false
      lookupCommand := "git", args := ["grep", "-n", "pattern"] }
  , { name := "curl_is_restricted_to_http", kernelContained := true
      lookupCommand := "curl", args := ["-fsS", "https://example.com"] }
  , { name := "uncontained_sed_runs_sandboxed", kernelContained := false
      lookupCommand := "sed", args := ["-n", "1,10p", "README.md"] }
  , { name := "contained_sed_runs_as_validated", kernelContained := true
      lookupCommand := "sed", args := ["-n", "1,10p", "README.md"] }
  , { name := "other_commands_run_as_validated", kernelContained := false
      lookupCommand := "cat", args := ["README.md"] } ]

/-- The arguments a validated read-only command runs with: production
`read_only_launch_args`. -/
@[conformance_family]
def commandReadOnlyLaunch : Family LaunchInput LaunchOutput where
  inputs := readOnlyLaunchCases
  model := fun i =>
    { args := CommandPolicy.readOnlyLaunchArgs i.kernelContained
        { command := i.lookupCommand, lookupCommand := i.lookupCommand, args := i.args } }

example : (readOnlyLaunchCases.find? (·.name == "git_diff_runs_without_external_diff_or_textconv")).map
    (fun i => (commandReadOnlyLaunch.model i).args) =
      some [ "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"
           , "-c", "log.showSignature=false", "--no-optional-locks"
           , "--no-pager", "diff", "--no-ext-diff", "--no-textconv", "--stat", "HEAD" ] := by
  native_decide

structure EnvInput where
  name : String
  envKey : CommandPolicy.EnvKey
  inputPresent : Bool
  inputName : String
  inputValue : String
  outputName : String
  deriving Lean.ToJson

structure EnvOutput where
  valueKind : Option CommandPolicy.EnvValue
  outputValue : Option String
  deriving Lean.ToJson

@[conformance_family]
def commandEnvFiltering : Family EnvInput EnvOutput where
  inputs := CommandPolicy.commandEnvCases.map fun c =>
    { name := c.name, envKey := c.envKey, inputPresent := c.inputPresent
      inputName := c.inputName, inputValue := c.inputValue, outputName := c.outputName }
  model := fun i =>
    let value := CommandPolicy.filteredEnv (CommandPolicy.envInput i.envKey i.inputPresent) i.envKey
    { valueKind := value
      outputValue := value.map (·.toRustValue i.inputValue) }

private def envCase? (name : String) : Option CommandPolicy.CommandEnvCase :=
  CommandPolicy.commandEnvCases.find? (·.name == name)

example : (envCase? "env_key_marker_dropped").map (fun c =>
    (c.inputName, c.expected.map (·.toRustValue c.inputValue))) =
      some ("OPENAI_API_KEY", none) := by native_decide

example : ["env_pager_forced_cat", "env_pager_absent_still_forced_cat"].all (fun n =>
    (envCase? n).map (fun c => c.expected.map (·.toRustValue c.inputValue)) ==
      some (some "cat")) = true := by native_decide

end Conformance.Contracts

import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Session.CommandPolicy.Cases
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
  deriving Lean.ToJson

structure SandboxOutput where
  decision : String
  sandbox : Option String
  denialReason : Option String
  deriving Lean.ToJson

/-- The sandbox a command mode runs under on a host that does or does not
enforce the workspace-write sandbox: production `select_sandbox`, given the
host fact its caller reads. -/
@[conformance_family]
def commandSandboxSelection : Family SandboxInput SandboxOutput where
  inputs := CommandPolicy.commandSandboxCases.map fun c =>
    { name := c.name, category := c.category, mode := c.mode
      workspaceWriteSandboxEnforced := c.workspaceWriteSandboxEnforced }
  model := fun i =>
    let decision := CommandPolicy.selectSandbox
      { workspaceWriteSandboxEnforced := i.workspaceWriteSandboxEnforced } i.mode
    { decision := decision.toContract
      sandbox := decision.sandbox?.map CommandPolicy.SandboxKind.toContract
      denialReason := decision.denialReason?.map CommandPolicy.DenialReason.toContract }

example : (CommandPolicy.commandSandboxCases.find?
    (·.name == "workspace_write_enforced_selects_macos_seatbelt")).map (·.decision) =
      some (.selected .macosSeatbelt) := by native_decide

example : (CommandPolicy.commandSandboxCases.find?
    (·.name == "unrestricted_selects_unsandboxed_unrestricted")).map (·.decision) =
      some (.selected .unsandboxedUnrestricted) := by native_decide

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

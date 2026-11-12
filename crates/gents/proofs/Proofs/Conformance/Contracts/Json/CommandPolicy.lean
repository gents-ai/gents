import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Session.CommandPolicy.Cases
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.Contracts

open Conformance.ContractCases

def commandPolicyCaseJson (witness : CommandPolicy.CommandPolicyCase) : String :=
  let reason := witness.decision.denialReason?
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"category\":" ++ jsonString witness.category ++ ","
    ++ "\"mode\":" ++ jsonString witness.policy.mode.toDefraDB ++ ","
    ++ "\"allowed_argv_prefixes\":"
      ++ jsonStringMatrix witness.policy.allowedArgvPrefixes ++ ","
    ++ "\"forbidden_argv_prefixes\":"
      ++ jsonStringMatrix witness.policy.forbiddenArgvPrefixes ++ ","
    ++ "\"network_mode\":" ++ jsonString witness.policy.networkMode.toDefraDB ++ ","
    ++ "\"read_only_allowlist\":"
      ++ jsonStringArray witness.policy.readOnlyAllowlist ++ ","
    ++ "\"command\":" ++ jsonString witness.request.command ++ ","
    ++ "\"lookup_command\":" ++ jsonString witness.request.lookupCommand ++ ","
    ++ "\"args\":" ++ jsonStringArray witness.request.args ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"denial_reason\":"
      ++ jsonOptionalString (reason.map CommandPolicy.DenialReason.toContract) ++ ","
    ++ "\"matched_prefix\":"
      ++ jsonOptionalStringArray (reason.bind CommandPolicy.DenialReason.matchedPrefix?) ++ ","
    ++ "\"denied_argv\":"
      ++ jsonOptionalStringArray (reason.bind CommandPolicy.DenialReason.argv?) ++ ","
    ++ "\"denied_command\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.command?) ++ ","
    ++ "\"denied_argument\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.argument?) ++ ","
    ++ "\"denied_subcommand\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.subcommand?)
    ++ "}"

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

def commandEnvCaseJson (witness : CommandPolicy.CommandEnvCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"env_key\":" ++ jsonString witness.envKey.toContract ++ ","
    ++ "\"input_present\":" ++ boolString witness.inputPresent ++ ","
    ++ "\"input_name\":" ++ jsonString witness.inputName ++ ","
    ++ "\"input_value\":" ++ jsonString witness.inputValue ++ ","
    ++ "\"output_name\":" ++ jsonString witness.outputName ++ ","
    ++ "\"expected_value_kind\":"
      ++ jsonOptionalString (witness.expected.map CommandPolicy.EnvValue.toContract) ++ ","
    ++ "\"expected_output_value\":"
      ++ jsonOptionalString (witness.expected.map (fun value => value.toRustValue witness.inputValue))
    ++ "}"

end Conformance.Contracts

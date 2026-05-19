import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.CommandPolicy.Cases

/-!
# Command Policy JSON

Serializers for command policy, sandbox, and environment witness rows.
-/

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

def commandSandboxCaseJson (witness : CommandPolicy.CommandSandboxCase) : String :=
  let reason := witness.decision.denialReason?
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"category\":" ++ jsonString witness.category ++ ","
    ++ "\"mode\":" ++ jsonString witness.mode.toDefraDB ++ ","
    ++ "\"workspace_write_sandbox_enforced\":"
      ++ boolString witness.workspaceWriteSandboxEnforced ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"sandbox\":"
      ++ jsonOptionalString ((witness.decision.sandbox?).map CommandPolicy.SandboxKind.toContract) ++ ","
    ++ "\"denial_reason\":"
      ++ jsonOptionalString (reason.map CommandPolicy.DenialReason.toContract)
    ++ "}"

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

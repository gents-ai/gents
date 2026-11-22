import Proofs.Shared.ToolPolicy.Presentation
import Proofs.Shared.AccountRotation.Policy
import Proofs.Shared.CompletionRetry.OutputObligation
import Proofs.Conformance.Family

namespace AccountRotation

deriving instance Lean.ToJson for Family, Claim, Overage

attribute [conformance_function] Claim.binds Overage.serves

end AccountRotation

namespace CompletionRetry.OutputObligation

structure GateConfiguration where
  obligations : List ConfiguredObligation

deriving instance Lean.ToJson for ConfiguredObligation, TerminalHandling, TerminalAnswer
deriving instance Lean.ToJson for GateConfiguration

@[conformance_family]
def loopTerminal : Conformance.Family GateConfiguration TerminalAnswer where
  inputs := terminalAnswerCases.map fun obligations => { obligations }
  model := fun configuration => terminalAnswer configuration.obligations

end CompletionRetry.OutputObligation

namespace ToolPresentation

deriving instance Lean.ToJson for Origin, AdmissionDecision

attribute [conformance_function] admissionDecision effectiveHidden configurationValid

@[conformance_family]
def gateways : Conformance.Family String Bool where
  inputs := ["script", "config", "load_skill", "spawn_process", "schema", "workflow", "query",
    "Script", "spawn_process_extra", ""]
  model := isGateway

end ToolPresentation

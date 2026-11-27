import Proofs.Shared.ToolPolicy.Presentation
import Proofs.Shared.AccountRotation.Policy
import Proofs.Shared.AccountRotation.Pool
import Proofs.Shared.CompletionRetry.OutputObligation
import Proofs.Conformance.Family

namespace AccountRotation

deriving instance Lean.ToJson for Family, Claim, Overage, LiveLimitStatus, Tier,
  CursorAction, CapStatus, SessionMapAction, Reselection

attribute [conformance_function] Claim.binds Overage.serves tierOfStatus Tier.rank
  cursorAction sessionMapAction markRotatedOnFailure

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
  listedAfterWiring

@[conformance_family]
def gateways : Conformance.Family String Bool where
  inputs := ["script", "config", "load_skill", "spawn_process", "schema", "workflow", "query",
    "Script", "spawn_process_extra", ""]
  model := isGateway

@[conformance_family]
def defaults : Conformance.Family String Bool where
  inputs := ["charts", "data_tables", "browser_exec", "script", "web_search", "web_fetch",
    "query", "Charts", "gents/charts", "chart", ""]
  model := defaultHidden

end ToolPresentation

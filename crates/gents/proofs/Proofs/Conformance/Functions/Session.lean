import Proofs.Conformance.Family
import Proofs.Session.Client.Types
import Proofs.Session.CommandPolicy.ArtifactAuthority
import Proofs.Session.CommandPolicy.Sandbox
import Proofs.Session.DurableLineage
import Proofs.Session.ManagedExec.Ownership
import Proofs.Session.Operations
import Proofs.Session.PromptAssembly.ClaudeCache
import Proofs.Session.Request.State
import Proofs.Session.RequestExecutionLease.State

deriving instance Lean.ToJson for ClientTurnState

deriving instance Lean.ToJson for RequestState

attribute [conformance_function] ClientTurnState.isTerminal

namespace CommandPolicy

deriving instance Lean.ToJson for ExecutionMode

attribute [conformance_function] ExecutionMode.meet protectsCredentials

end CommandPolicy

namespace ManagedExec

deriving instance Lean.ToJson for ProcessObservation, StopOutcome, CancelReply

attribute [conformance_function] mayTerminate retainProcessRecord cancelReply stopOutcome

end ManagedExec

namespace DurableLineage

attribute [conformance_function] deferredCreationAllowed

end DurableLineage

namespace PromptAssembly.ClaudeCache

deriving instance Lean.ToJson for Ttl

attribute [conformance_function] ttl

end PromptAssembly.ClaudeCache

namespace RequestExecutionLease

deriving instance Lean.ToJson for Outcome

attribute [conformance_function] Outcome.requestState providerEofIsFailure

end RequestExecutionLease

namespace SessionOperations

deriving instance Lean.ToJson for DeliveryChoice, Delivery, Matches, Lookup, ReissueKind,
  ReissueParent, ReissueDecision

attribute [conformance_function] resolveDelivery requestLookup reissueDecision interruptSettled

end SessionOperations

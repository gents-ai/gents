import Proofs.Session.TranscriptInputOrigin
import Proofs.Conformance.Family
import Proofs.Session.AgentSession
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

namespace AgentSession

deriving instance Lean.ToJson for KeyOrder

attribute [conformance_function] newerByKeys

end AgentSession

namespace CommandPolicy

deriving instance Lean.ToJson for ExecutionMode

attribute [conformance_function] ExecutionMode.meet protectsCredentials

end CommandPolicy

namespace ManagedExec

deriving instance Lean.ToJson for ProcessObservation, StopOutcome, CancelReply

attribute [conformance_function] mayTerminate retainProcessRecord cancelReply stopOutcome

end ManagedExec

namespace DurableLineage

deriving instance Lean.ToJson for CauseLoad, ResultRead, EffectRead, CreationOutcome

attribute [conformance_function] creationVerdict

end DurableLineage

namespace PromptAssembly.ClaudeCache

deriving instance Lean.ToJson for Ttl

attribute [conformance_function] ttl

end PromptAssembly.ClaudeCache

namespace RequestExecutionLease

deriving instance Lean.ToJson for Outcome

attribute [conformance_function] Outcome.requestState providerEofIsFailure retryWakeDispatches

end RequestExecutionLease

namespace SessionOperations

deriving instance Lean.ToJson for DeliveryChoice, Delivery, Matches, Lookup, ReissueKind,
  ReissueParent, ReissueDecision

attribute [conformance_function] resolveDelivery requestLookup reissueDecision interruptSettled

end SessionOperations

namespace TranscriptInputOrigin

deriving instance Lean.ToJson for Kind

attribute [conformance_function] classification

end TranscriptInputOrigin

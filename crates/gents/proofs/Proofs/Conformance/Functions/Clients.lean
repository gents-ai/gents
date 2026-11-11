import Proofs.Clients.ClientShell.ChatReadiness
import Proofs.Clients.ClientShell.ObservationOrdering
import Proofs.Clients.ClientShell.Settings
import Proofs.Clients.ClientShell.SyncHealth
import Proofs.Conformance.Family

namespace ClientChatReadiness

deriving instance Lean.ToJson for Cause, NextAction

attribute [conformance_function] nextAction

end ClientChatReadiness

namespace ClientShell.Settings

attribute [conformance_function] adoptSavedProjection

end ClientShell.Settings

namespace ClientSyncHealth

deriving instance Lean.ToJson for Cause, NextAction

attribute [conformance_function] Cause.nextAction

end ClientSyncHealth

namespace RuntimeConnectionObservation

deriving instance Lean.ToJson for Event, SnapshotOutcome, Origin

attribute [conformance_function] armedEvent armedAfter snapshotEvent reconnects

end RuntimeConnectionObservation

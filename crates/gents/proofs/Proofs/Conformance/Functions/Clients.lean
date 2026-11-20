import Proofs.Clients.AuthoredOutbox
import Proofs.Clients.RuntimeAccess
import Proofs.Clients.ClientShell.ChatReadiness
import Proofs.Clients.ClientShell.ObservationOrdering
import Proofs.Clients.ClientShell.Settings
import Proofs.Clients.ClientShell.SyncHealth
import Proofs.Clients.ServiceLifecycle
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Store

namespace ClientChatReadiness

deriving instance Lean.ToJson for Cause, NextAction

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

attribute [conformance_function] armedEvent armedAfter snapshotEvent selectedRootSnapshot failedSnapshot reconnects

end RuntimeConnectionObservation

namespace ObserverResync

deriving instance Lean.ToJson for Change, Delivery

attribute [conformance_function] deliver

end ObserverResync

namespace ServiceLifecycle

deriving instance Lean.ToJson for Action, Decision, StartDisposition

attribute [conformance_function] Action.disrupts Action.contendsWithUpdate admission startDisposition

end ServiceLifecycle

attribute [conformance_function] ClientAuthoredOutbox.acknowledge

namespace ClientRuntimeAccess

deriving instance Lean.ToJson for RequestAdmission

attribute [conformance_function] admitRequest

end ClientRuntimeAccess

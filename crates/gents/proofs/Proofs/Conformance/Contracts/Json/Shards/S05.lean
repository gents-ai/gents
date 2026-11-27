import Proofs.Conformance.Contracts.Json.Vocabularies
import Proofs.Conformance.ApplicationPairing
import Proofs.Conformance.AuxiliaryOutput
import Proofs.Conformance.CaptureScope
import Proofs.Conformance.Contracts.Json.ClaudePlatform
import Proofs.Conformance.Contracts.Json.PackInstallation
import Proofs.Conformance.Contracts.Json.TerminalSelection
import Proofs.Conformance.GoalClientMutation
import Proofs.Conformance.Identity.ClientTransport
import Proofs.Conformance.Identity.HouseholdTools
import Proofs.Conformance.Identity.NetworkProtocol
import Proofs.Conformance.InterruptRequestDrain
import Proofs.Conformance.PairingTick
import Proofs.Conformance.PluginJobDispatch
import Proofs.Conformance.RootAdmission
import Proofs.Conformance.RootCandidates.ProviderSanitation
import Proofs.Conformance.SessionQueueOrderCases
import Proofs.Conformance.Triggers.ScheduleOccurrence
import Proofs.Store.Identity.ClientAgentCreation

namespace Conformance.Contracts.Shards.S05

set_option maxHeartbeats 1000000 in
conformance_shard_entries entries

end Conformance.Contracts.Shards.S05

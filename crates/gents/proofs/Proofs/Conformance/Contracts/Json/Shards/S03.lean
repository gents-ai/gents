import Proofs.Conformance.Contracts.Json.Vocabularies
import Proofs.Conformance.BaseFreezeRecovery
import Proofs.Conformance.Contracts.Json.BackgroundWakeRows
import Proofs.Conformance.Contracts.Json.InterruptQueue
import Proofs.Conformance.Functions.Runtime
import Proofs.Conformance.GoalClientDelivery
import Proofs.Conformance.Identity.ClientAgentCreation
import Proofs.Conformance.Identity.GenesisReadAccess
import Proofs.Conformance.Initialization
import Proofs.Conformance.PairingOwnedDiff
import Proofs.Conformance.PluginJob
import Proofs.Conformance.RequestRepairScope
import Proofs.Conformance.RootCandidates.EvalSelection
import Proofs.Conformance.SessionInputEditMalformed
import Proofs.Conformance.TaskHooks
import Proofs.Conformance.TitleUsage
import Proofs.Conformance.ToolSurfaceArtifact
import Proofs.Store.Identity.AgentBlockSigning

namespace Conformance.Contracts.Shards.S03

set_option maxHeartbeats 1000000 in
conformance_shard_entries entries

end Conformance.Contracts.Shards.S03

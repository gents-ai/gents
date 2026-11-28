import Proofs.Conformance.Contracts.Json.Vocabularies
import Proofs.Conformance.AccountStore
import Proofs.Conformance.ClientAgentReadiness
import Proofs.Conformance.ContractCases.CompactionProjectionJoin
import Proofs.Conformance.Contracts.Json.CausalHop
import Proofs.Conformance.Contracts.Json.ExecutionGate
import Proofs.Conformance.Contracts.Json.SelfConfig
import Proofs.Conformance.CountCarrier
import Proofs.Conformance.EnrollmentRenewalNotices
import Proofs.Conformance.Functions.Shared
import Proofs.Conformance.GoalRequestAncestry
import Proofs.Conformance.Identity.BlockScope
import Proofs.Conformance.Identity.HouseholdCToolGrants
import Proofs.Conformance.Identity.Publication
import Proofs.Conformance.Identity.WorkspaceRoot
import Proofs.Conformance.NativeCompletionTrace
import Proofs.Conformance.PluginHostCalls
import Proofs.Conformance.ReceiptRecovery
import Proofs.Conformance.ScheduleCadence
import Proofs.Conformance.StreamBatch

namespace Conformance.Contracts.Shards.S20

set_option maxHeartbeats 1000000 in
conformance_shard_entries entries

end Conformance.Contracts.Shards.S20

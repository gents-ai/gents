import Proofs.Conformance.Contracts.Json.Vocabularies
import Proofs.Automation.EventDelivery.WatcherConformance
import Proofs.Conformance.BackendPreflight
import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.Contracts.Json.Callback
import Proofs.Conformance.DocumentAcpChoice
import Proofs.Conformance.Functions.Inference
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.HeldAudience
import Proofs.Conformance.Hooks
import Proofs.Conformance.Identity.AgentCreationReceipt
import Proofs.Conformance.Identity.SessionInterruptAccess
import Proofs.Conformance.MailboxReply
import Proofs.Conformance.OperatorPaths
import Proofs.Conformance.ProviderViewCallKeys
import Proofs.Conformance.RootCandidates.StartupDegraded
import Proofs.Conformance.StateMachines
import Proofs.Conformance.UserDelegation

namespace Conformance.Contracts.Shards.S16

set_option maxHeartbeats 1000000 in
conformance_shard_entries entries

end Conformance.Contracts.Shards.S16

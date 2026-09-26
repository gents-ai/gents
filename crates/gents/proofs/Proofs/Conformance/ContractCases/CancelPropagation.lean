import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

/-- Authored integration expectations for an explicit bridge cancellation:
the replicated child interrupt intent and its acknowledgment do not imply that
the child has terminalized. -/
def cancelPropagationCases : List CancelPropagationCase :=
  [ { name := "cancel_propagates_across_declarative_subagent_legs"
    , route := "declarative_subagent_pairing"
    , action := "cancel_bridge"
    , parentPrincipal := "coordinator"
    , childPrincipal := "worker"
    , parentRequestId := "cancel-lean-parent"
    , parentToolCallId := "cancel-lean-tool"
    , childRequestId := "cancel-lean-child"
    , bridgeCollection := "AgentToolCall"
    , childRequestCollection := "AgentRequest"
    , cancelIntentWrittenOnBridge := true
    , bridgeCancelReplicatesToHost := true
    , hostInterruptsChild := true
    , childInterruptIntentReplicatesToCoordinator := true
    , cancelAckReturnsToCoordinator := true
    , noThirdPartyRows := true } ]

end Conformance.ContractCases

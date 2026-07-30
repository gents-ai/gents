import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

def cancelPropagationCases : List CancelPropagationCase :=
  [ { name := "cancel_propagates_across_declarative_subagent_legs"
    , route := "declarative_subagent_pairing"
    , action := "cancel_parent"
    , parentDeployment := "coordinator"
    , childDeployment := "host"
    , parentRequestId := "cancel-lean-parent"
    , parentToolCallId := "cancel-lean-tool"
    , childRequestId := "cancel-lean-child"
    , bridgeCollection := "AgentToolCall"
    , childRequestCollection := "AgentRequest"
    , cancelIntentWrittenOnBridge := true
    , bridgeCancelReplicatesToHost := true
    , hostInterruptsChild := true
    , childTerminalReplicatesToCoordinator := true
    , cancelAckReturnsToCoordinator := true
    , noThirdPartyRows := true } ]

end Conformance.ContractCases

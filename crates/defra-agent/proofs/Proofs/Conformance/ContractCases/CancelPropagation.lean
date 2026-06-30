import Proofs.Conformance.ContractCases.Types

/-!
# Declarative Subagent Cancel-Propagation Conformance Case

Finite witness for the directional declarative subagent pairing topology. The
row pins the observable contract: coordinator-owned bridge cancel intent travels
to the host, the host interrupts the child, and the host-owned terminal/ack
returns to the coordinator without broad third-party replication.
-/

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

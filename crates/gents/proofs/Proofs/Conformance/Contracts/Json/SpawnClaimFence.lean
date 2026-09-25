import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.SpawnClaimFence

namespace Conformance.Contracts

open Conformance.ContractCases

def spawnFenceStepJson (value : SpawnFenceStep) : String :=
  "{\"action\":" ++ jsonString value.action
    ++ ",\"enabled\":" ++ boolString value.enabled
    ++ ",\"stale_host_view\":" ++ boolString value.staleHostView
    ++ ",\"bridge\":" ++ jsonString value.bridge
    ++ ",\"child\":" ++ jsonString value.child
    ++ ",\"cancel_intent\":" ++ boolString value.cancelIntent
    ++ ",\"host_sees_intent\":" ++ boolString value.hostSeesIntent
    ++ ",\"interrupt_latched\":" ++ boolString value.interruptLatched
    ++ ",\"ack_pending\":" ++ boolString value.ackPending
    ++ "}"

def spawnFenceCaseJson (value : SpawnFenceCase) : String :=
  "{\"name\":" ++ jsonString value.name
    ++ ",\"route\":" ++ jsonString value.route
    ++ ",\"unclaimed_deadline_set\":" ++ boolString value.unclaimedDeadlineSet
    ++ ",\"single_node_replayable\":" ++ boolString value.singleNodeReplayable
    ++ ",\"steps\":" ++ jsonArray (value.steps.map spawnFenceStepJson)
    ++ "}"

def spawnFenceCasesJson : String :=
  jsonArray (spawnFenceCases.map spawnFenceCaseJson)

end Conformance.Contracts

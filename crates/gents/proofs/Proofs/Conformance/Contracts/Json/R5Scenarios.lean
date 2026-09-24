import Proofs.Conformance.ContractCases.R5Scenarios
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts

open Conformance.ContractCases

def r5NodeString : R5Node → String | .a => "A" | .b => "B"
def r5TerminalString : R5Terminal → String
  | .completed => "completed" | .failed => "failed" | .interrupted => "interrupted"

def r5AckJson (event : String × Subagent.CancelAcknowledgement.Outcome) : String :=
  let outcome := match event.2 with
    | .pending => "pending" | .stuck => "stuck" | .acked => "acked"
  "{\"tool\":" ++ jsonString event.1 ++ ",\"outcome\":" ++ jsonString outcome ++ "}"

def r5BridgeFactJson (bridge : R5BridgeFact) : String :=
  "{\"tool\":" ++ jsonString bridge.tool ++
  ",\"child\":" ++ jsonString bridge.child ++
  ",\"state\":" ++ jsonString bridge.state.toDefraDB ++ "}"

def r5ChildFactJson (child : R5ChildFact) : String :=
  "{\"child\":" ++ jsonString child.child ++
  ",\"terminal\":" ++ (match child.terminal with
    | none => "null"
    | some terminal => jsonString (r5TerminalString terminal)) ++
  ",\"interrupt_requested\":" ++ boolString child.interruptRequested ++ "}"

def r5ActionJson : R5ScenarioAction → String
  | .pair node peer => "{\"op\":\"PairPrincipals\",\"node\":" ++ jsonString (r5NodeString node) ++ ",\"peer\":" ++ jsonString (r5NodeString peer) ++ "}"
  | .acceptedBridge tool child session parentDepth => "{\"op\":\"PublishAcceptedBackgroundBridge\",\"tool\":" ++ jsonString tool ++ ",\"child\":" ++ jsonString child ++ ",\"session\":" ++ jsonString session ++ ",\"parent_depth\":" ++ toString parentDepth ++ "}"
  | .rejectedSpawnInvocation tool child session parentDepth => "{\"op\":\"RejectSpawnInvocation\",\"tool\":" ++ jsonString tool ++ ",\"child\":" ++ jsonString child ++ ",\"session\":" ++ jsonString session ++ ",\"parent_depth\":" ++ toString parentDepth ++ "}"
  | .replicateBridge tool source target => "{\"op\":\"ReplicateBridge\",\"tool\":" ++ jsonString tool ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .materializeChild child tool => "{\"op\":\"MaterializeChild\",\"child\":" ++ jsonString child ++ ",\"tool\":" ++ jsonString tool ++ "}"
  | .beginChild child generation => "{\"op\":\"BeginChild\",\"child\":" ++ jsonString child ++ ",\"generation\":" ++ toString generation ++ "}"
  | .awaitChildExpiry child => "{\"op\":\"AwaitChildExpiry\",\"child\":" ++ jsonString child ++ "}"
  | .replicateChild child source target => "{\"op\":\"ReplicateChild\",\"child\":" ++ jsonString child ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .publishTerminal child terminal hasMessage => "{\"op\":\"PublishChildTerminal\",\"child\":" ++ jsonString child ++ ",\"terminal\":" ++ jsonString (r5TerminalString terminal) ++ ",\"has_message\":" ++ boolString hasMessage ++ "}"
  | .replicateTerminalRequest child source target => "{\"op\":\"ReplicateTerminalRequest\",\"child\":" ++ jsonString child ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .replicateOutputSegments child source target => "{\"op\":\"ReplicateOutputSegments\",\"child\":" ++ jsonString child ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .replicateMessageHeader child source target => "{\"op\":\"ReplicateMessageHeader\",\"child\":" ++ jsonString child ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .observeCompletion => "{\"op\":\"ObserveCompletion\"}"
  | .cancelBridge tool => "{\"op\":\"CancelBridge\",\"tool\":" ++ jsonString tool ++ "}"
  | .replicateCancelIntent tool source target => "{\"op\":\"ReplicateCancelIntent\",\"tool\":" ++ jsonString tool ++ ",\"from\":" ++ jsonString (r5NodeString source) ++ ",\"to\":" ++ jsonString (r5NodeString target) ++ "}"
  | .mirrorCancel tool => "{\"op\":\"MirrorCancel\",\"tool\":" ++ jsonString tool ++ "}"
  | .observeCancelAck => "{\"op\":\"ObserveCancelAck\"}"
  | .recoverBridges => "{\"op\":\"RecoverBridges\"}"
  | .recoverChildRequests expected fresh => "{\"op\":\"RecoverChildRequests\",\"expected_generation\":" ++ toString expected ++ ",\"fresh_generation\":" ++ toString fresh ++ "}"
  | .crash node premise => "{\"op\":\"CrashNode\",\"node\":" ++ jsonString (r5NodeString node) ++ ",\"durable_reopen_premise\":" ++ boolString premise ++ "}"
  | .advanceClock node seconds => "{\"op\":\"AdvanceClock\",\"node\":" ++ jsonString (r5NodeString node) ++ ",\"seconds\":" ++ toString seconds ++ "}"
  | .converge => "{\"op\":\"Converge\"}"

def r5RecoveryCheckpointJson (checkpoint : Nat × R5ScenarioState) : String :=
  "{\"after_action\":" ++ toString checkpoint.1 ++
  ",\"notification_children\":" ++ jsonArray (checkpoint.2.notifications.map jsonString) ++
  ",\"wake_sessions\":" ++ jsonArray (checkpoint.2.wakeSessions.map jsonString) ++
  ",\"bridges\":" ++ jsonArray (checkpoint.2.aBridges.map r5BridgeFactJson) ++
  ",\"children\":" ++ jsonArray (checkpoint.2.bChildren.map r5ChildFactJson) ++ "}"

def r5ScenarioCaseJson (scenario : R5ScenarioCase) : String :=
  "{\"name\":" ++ jsonString scenario.name ++
  ",\"child_lease_secs\":" ++ toString scenario.childLeaseSecs ++
  ",\"cancel_ack_threshold_secs\":" ++ toString scenario.post.cancelAckThreshold ++
  ",\"actions\":" ++ jsonArray (scenario.actions.map r5ActionJson) ++
  ",\"recovery_checkpoints\":" ++ jsonArray ((r5RecoveryCheckpoints scenario).map r5RecoveryCheckpointJson) ++
  ",\"expected_notifications\":" ++ toString scenario.post.notifications.length ++
  ",\"expected_wakes\":" ++ toString scenario.post.wakeSessions.length ++
  ",\"expected_rejected_invocations\":" ++ toString scenario.post.rejectedInvocations.length ++
  ",\"expected_cancel_ack_events\":" ++ jsonArray (scenario.post.cancelAcks.map r5AckJson) ++
  ",\"expected_a_bridges\":" ++ jsonArray (scenario.post.aBridges.map r5BridgeFactJson) ++
  ",\"expected_b_children\":" ++ jsonArray (scenario.post.bChildren.map r5ChildFactJson) ++
  ",\"expected_a_generation\":" ++ toString scenario.post.aGeneration ++
  ",\"expected_b_generation\":" ++ toString scenario.post.bGeneration ++ "}"

def r5ScenarioCasesJson : String := jsonArray (r5ScenarioCases.map r5ScenarioCaseJson)

end Conformance.Contracts

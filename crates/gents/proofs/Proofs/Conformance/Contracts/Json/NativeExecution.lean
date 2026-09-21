import Proofs.CanonicalOutput.Execution.GateCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.RequestExecutionLease

namespace Conformance.NativeExecutionContracts

open CanonicalOutput
open CanonicalOutput.Execution
open CanonicalOutput.Execution.Examples
open CanonicalOutput.Execution.Gate
open CanonicalOutput.Execution.Gate.Cases
open Conformance.Contracts

/-- Compact native inputs. Each constructor names an existing execution-owner
operation and its externally supplied fence values; fixture documents are the
canonical `Execution.Examples` documents, not an encoded copy of `World`. -/
inductive Input where
  | acceptForeground
  | acceptRemote
  | dispatch (now : Nat)
  | closeForeground
  | deliverForeground
  | terminalizeCompleted
  | recover (actor : Nat) (now fresh deadline : Nat)

def Input.operation : Input → Operation
  | .acceptForeground => .accept 7 providerTurn providerMessage [] [foregroundAdmission]
  | .acceptRemote => .accept 7 providerTurn providerMessage [remote] [remoteAdmission]
  | .dispatch _ => .dispatch 7 permit
  | .closeForeground => .toolClose 600 (.native .complete) toolOutputClose
  | .deliverForeground => .toolDeliver 600 (foregroundResultMessage 1)
  | .terminalizeCompleted => .terminalize 7 .completed (.message 501)
  | .recover _ _ fresh deadline => .recover 7 fresh 5 deadline []

def Input.actor : Input → Nat
  | .recover actor .. => actor
  | _ => 1

def Input.now : Input → Nat
  | .dispatch now => now
  | .recover _ now .. => now
  | _ => 5

def Input.tag : Input → String
  | .acceptForeground => "accept_foreground"
  | .acceptRemote => "accept_remote"
  | .dispatch _ => "dispatch"
  | .closeForeground => "close_foreground_tool"
  | .deliverForeground => "deliver_foreground_result"
  | .terminalizeCompleted => "terminalize_completed"
  | .recover .. => "recover_expired_generation"

structure Observation where
  accepted : Bool
  generation : Option Nat
  requestState : String
  toolState : Option String
  toolStuckSince : Option Nat
  toolCancelIntentAt : Option Nat
  inFlight : Bool
  messageCount : Nat
  nextSequence : Nat
  acceptedSequence : Option Nat
  physicalToolRequest : Option Nat
  leaseDeadline : Option Nat
  deriving DecidableEq

def observe (document : Nat) (accepted : Bool) (world : World) : Observation :=
  let tool := ownedToolByDocument? world document
  { accepted
    generation := world.currentGeneration?
    requestState := world.lease.request.toDefraDB
    toolState := tool.map (ToolExecution.ToolCallState.toDefraDB ·.context.state)
    toolStuckSince := tool.bind (·.stuckSince)
    toolCancelIntentAt := tool.bind (·.cancelCascadeIntentAt)
    inFlight := document ∈ world.transcript.inFlight
    messageCount := world.messages.length
    nextSequence := world.transcript.nextSeq
    acceptedSequence := tool.map (·.acceptedSequence)
    physicalToolRequest := tool.map (·.requestDoc)
    leaseDeadline := match world.lease.lease with
      | .active _ _ deadline | .recoverable _ _ deadline => some deadline
      | _ => none }

def releaseFor (world : World) (actor : Nat) : Option World :=
  if world.gateOwner.isSome then scheduling world actor .release else some world

/-- Run each input through the real modeled gate. A rejected operation records
`accepted = false` and leaves the durable world unchanged, matching rollback. -/
def runStep (document : Nat) (world : World) (previousActor : Nat)
    (input : Input) : Option (World × Nat × Observation) := do
  let released ← releaseFor world previousActor
  let held ← acquire released input.actor true
  match commit held input.actor input.now input.operation with
  | some after => some (after, input.actor, observe document true after)
  | none => some (held, input.actor, observe document false held)

def run (seed : World) (document : Nat) (inputs : List Input) : Option (List Observation) := do
  let (_, _, observations) ← inputs.foldlM (fun (world, actor, observations) input => do
    let (after, nextActor, observation) ← runStep document world actor input
    pure (after, nextActor, observations ++ [observation])) (initial seed, 1, [])
  pure observations

structure Case where
  name : String
  seed : World
  queryDocument : Nat
  inputs : List Input
  expected : Option (List Observation)

def mkCase (name : String) (world : World) (inputs : List Input) : Case :=
  ⟨name, world, 600, inputs, run world 600 inputs⟩

def cases : List Case :=
  [ mkCase "pending_remote_recovery_cancels_before_dispatch"
      (routedWorld 5) [.acceptRemote, .recover 2 10 8 20, .dispatch 10]
  , mkCase "running_foreground_recovery_records_handoff"
      (world 5) [.acceptForeground, .dispatch 5, .recover 2 10 8 20]
  , mkCase "running_foreground_terminalization_rejected"
      (world 5) [.acceptForeground, .dispatch 5, .terminalizeCompleted]
  , mkCase "foreground_close_delivery_then_terminalization"
      (world 5) [.acceptForeground, .dispatch 5, .closeForeground,
        .deliverForeground, .terminalizeCompleted] ]

def admissionJson (value : ToolAdmission) : String :=
  "{" ++ "\"document\":" ++ toString value.document ++ ","
    ++ "\"call_id\":" ++ toString value.context.callId ++ ","
    ++ "\"request_id\":" ++ toString value.context.requestId ++ ","
    ++ "\"state\":" ++ jsonString value.context.state.toDefraDB ++ ","
    ++ "\"operation\":" ++ jsonString value.context.operation.toDefraDB ++ ","
    ++ "\"deadline\":" ++ toString value.context.deadline ++ ","
    ++ "\"started_at\":" ++ jsonOptionalNat value.context.startedAt ++ ","
    ++ "\"current_time\":" ++ toString value.context.currentTime ++ ","
    ++ "\"failure_class\":" ++
      (value.context.failureClass.map (jsonString ∘ ToolExecution.FailureClass.toDefraDB)).getD "null" ++ ","
    ++ "\"persistence\":" ++ jsonString value.context.persistence.toDefraDB ++ ","
    ++ "\"await_mode\":" ++ jsonString value.context.awaitMode.toDefraDB ++ ","
    ++ "\"cancel_policy\":" ++ jsonString value.context.cancelPolicy.toDefraDB ++
      ",\"child_request_id\":" ++
      jsonOptionalNat value.context.childRequestId ++ "}"

def targetJson (value : RemoteTarget) : String :=
  "{\"call\":" ++ toString value.call ++ ",\"coordinator\":" ++
    toString value.coordinator ++ ",\"target\":" ++ toString value.target ++ "}"

def seedJson (value : World) : String :=
  "{" ++ "\"request_id\":" ++ toString value.requestId ++ ","
    ++ "\"session_id\":" ++ toString value.sessionId ++ ","
    ++ "\"principal\":" ++ toString value.principal ++ ","
    ++ "\"remote_routes\":" ++ jsonArray (value.remoteRoutes.map fun (call, target) =>
      "{\"call\":" ++ toString call ++ ",\"target\":" ++ toString target ++ "}") ++ ","
    ++ "\"lease\":" ++ Conformance.RequestExecutionLeaseContracts.worldJson value.lease ++ ","
    ++ "\"transcript_session_id\":" ++ toString value.transcript.sessionId ++ ","
    ++ "\"next_sequence\":" ++ toString value.transcript.nextSeq ++ ","
    ++ "\"segments\":[],\"messages\":[],\"tool_calls\":[],\"in_flight\":[]}"

def inputJson (input : Input) : String :=
  let common := "{\"operation\":" ++ jsonString input.tag ++
    ",\"actor\":" ++ toString input.actor ++ ",\"now\":" ++ toString input.now
  match input.operation with
  | .accept generation closing message targets admissions =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"closing\":" ++
        canonicalSegmentJson closing ++ ",\"message\":" ++ canonicalMessageJson message ++
        ",\"targets\":" ++ jsonArray (targets.map targetJson) ++ ",\"admissions\":" ++
        jsonArray (admissions.map admissionJson) ++ "}"
  | .dispatch generation permit =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"call\":" ++
        toString permit.call ++ ",\"cancellation_allows\":" ++
        jsonOptionalBool (some permit.cancellationAllows) ++ ",\"tool_policy_allows\":" ++
        jsonOptionalBool (some permit.toolPolicyAllows) ++ "}"
  | .toolClose document (.native .complete) record =>
      common ++ ",\"document\":" ++ toString document ++
        ",\"authority_outcome\":\"complete\",\"record\":" ++
        canonicalSegmentJson record ++ "}"
  | .toolDeliver document message =>
      common ++ ",\"document\":" ++ toString document ++ ",\"message\":" ++
        canonicalMessageJson message ++ "}"
  | .terminalize generation outcome (.message id) =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"outcome\":" ++
        jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName outcome) ++
        ",\"selection\":{\"kind\":\"message\",\"id\":" ++
        toString id ++ "}}"
  | .recover expected fresh duration deadline [] =>
      common ++ ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++ ",\"duration\":" ++
        toString duration ++ ",\"deadline\":" ++ toString deadline ++
        ",\"items\":[]}"
  | _ => "null"

def observationJson (value : Observation) : String :=
  "{" ++ "\"accepted\":" ++ jsonOptionalBool (some value.accepted) ++ ","
    ++ "\"generation\":" ++ jsonOptionalNat value.generation ++ ","
    ++ "\"request_state\":" ++ jsonString value.requestState ++ ","
    ++ "\"tool_state\":" ++ (value.toolState.map jsonString).getD "null" ++ ","
    ++ "\"tool_stuck_since\":" ++ jsonOptionalNat value.toolStuckSince ++ ","
    ++ "\"tool_cancel_intent_at\":" ++ jsonOptionalNat value.toolCancelIntentAt ++ ","
    ++ "\"in_flight\":" ++ jsonOptionalBool (some value.inFlight) ++ ","
    ++ "\"message_count\":" ++ toString value.messageCount ++ ","
    ++ "\"next_sequence\":" ++ toString value.nextSequence ++ ","
    ++ "\"accepted_sequence\":" ++ jsonOptionalNat value.acceptedSequence ++ ","
    ++ "\"physical_tool_request\":" ++ jsonOptionalNat value.physicalToolRequest ++ ","
    ++ "\"lease_deadline\":" ++ jsonOptionalNat value.leaseDeadline ++ "}"

def caseJson (value : Case) : String :=
  "{" ++ "\"kind\":\"native_execution\","
    ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"seed\":" ++ seedJson value.seed ++ ","
    ++ "\"query_document\":" ++ toString value.queryDocument ++ ","
    ++ "\"operations\":" ++ jsonArray (value.inputs.map inputJson) ++ ","
    ++ "\"expected_observations\":" ++
      (value.expected.map (jsonArray ∘ List.map observationJson)).getD "null" ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.all (fun value => value.expected.isSome) = true := by native_decide

/-- Collections represented as empty, plus omitted optional execution state, are
empty in every modeled seed. -/
example : cases.all (fun value => value.seed.segments.isEmpty && value.seed.messages.isEmpty &&
    value.seed.transcript.messages.isEmpty && value.seed.transcript.toolCalls.isEmpty &&
    value.seed.transcript.inFlight == ∅ && value.seed.compactionCursor.isNone &&
    value.seed.toolContexts.isEmpty && value.seed.delegatedCalls.isEmpty &&
    value.seed.terminalSelection.isNone && value.seed.gateOwner.isNone &&
    value.seed.claimed.isNone) = true := by
  native_decide

/-- Every script is substantive, serializable through its modeled operations,
and produces exactly one observation for each attempted operation. -/
example : cases.all (fun value => !value.inputs.isEmpty &&
    (value.inputs.map inputJson).all (· != "null") &&
    match value.expected with
    | some observations => observations.length == value.inputs.length
    | none => false) = true := by
  native_decide

end Conformance.NativeExecutionContracts
